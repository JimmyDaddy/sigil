#!/usr/bin/env node

import { createHash, randomUUID } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  chmod,
  lstat,
  mkdir,
  open,
  readFile,
  readlink,
  readdir,
  realpath,
  rename,
  rm,
  stat,
  truncate,
} from "node:fs/promises";
import { createReadStream } from "node:fs";
import { hostname } from "node:os";
import { basename, dirname, join, relative, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";

const LEDGER_SCHEMA_VERSION = 1;
const LEDGER_KIND = "sigil-desktop-notarization-ledger";
const NOTARY_DIRECTORY = ".notary";
const LEDGER_NAME = "events.jsonl";
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const TERMINAL_STATUSES = new Set(["Accepted", "Invalid", "Rejected"]);

export async function initializeLedger({ root, tag, commit, teamId, profile }) {
  return mutateLedger(root, async (state) => {
    validateReleaseIdentity({ tag, commit, teamId, profile });
    const release = { tag, version: tag.slice(1), commit, teamId, profile };
    if (state.release) {
      if (JSON.stringify(state.release) !== JSON.stringify(release)) {
        throw new Error("notarization ledger belongs to a different release identity");
      }
      return state;
    }
    await appendEvent(root, { type: "release_created", release });
    return projectLedger(root);
  });
}

export async function registerArtifact({
  root,
  key,
  kind,
  target,
  expectedArch,
  submissionPath,
  staplePath,
}) {
  return mutateLedger(root, async (state) => {
    requireRelease(state);
    validateArtifactIdentity({ key, kind, target, expectedArch });
    const normalizedSubmissionPath = normalizeOwnedPath(root, submissionPath);
    const normalizedStaplePath = normalizeOwnedPath(root, staplePath);
    await assertConfinedPath(root, normalizedSubmissionPath.absolute);
    await assertConfinedPath(root, normalizedStaplePath.absolute, true);
    const submissionStat = await lstat(normalizedSubmissionPath.absolute);
    if (!submissionStat.isFile()) {
      throw new Error(`notarization submission must be a regular file: ${submissionPath}`);
    }
    const artifact = {
      key,
      kind,
      target,
      expectedArch,
      submissionPath: normalizedSubmissionPath.relative,
      staplePath: normalizedStaplePath.relative,
      submissionName: basename(normalizedSubmissionPath.absolute),
      bytes: submissionStat.size,
      sha256: await sha256File(normalizedSubmissionPath.absolute),
    };
    const existing = state.artifacts.get(key);
    if (existing) {
      if (JSON.stringify(existing.registration) !== JSON.stringify(artifact)) {
        throw new Error(`notarization artifact registration changed: ${key}`);
      }
      return state;
    }
    await appendEvent(root, { type: "artifact_registered", artifact });
    return projectLedger(root);
  });
}

export async function submitRegisteredArtifacts(root, options = {}) {
  return mutateLedger(root, async (state) => {
    requireRelease(state);
    await assertSubmissionBytes(root, state);
    const keys = options.keys ?? [...state.artifacts.keys()];
    const runner = options.runner ?? runNotaryTool;
    for (const key of keys) {
      state = await projectLedger(root);
      const artifact = requireArtifact(state, key);
      if (artifact.activeAttempt) {
        continue;
      }
      const historyResult = await runner([
        "history",
        "--keychain-profile",
        state.release.profile,
        "--output-format",
        "json",
      ]);
      const history = parseNotaryResponse(historyResult, "history");
      const attemptId = randomUUID();
      const startedAt = new Date().toISOString();
      await appendEvent(root, {
        type: "submission_started",
        artifactKey: key,
        attemptId,
        startedAt,
        historyIdsBefore: historyEntries(history).map((entry) => entry.id),
      });

      const args = [
        "submit",
        resolveOwnedPath(root, artifact.registration.submissionPath),
        "--keychain-profile",
        state.release.profile,
        "--output-format",
        "json",
      ];
      if (options.webhook) {
        validateWebhook(options.webhook);
        args.push("--webhook", options.webhook);
      }
      const result = await runner(args);
      let response;
      try {
        response = parseNotaryResponse(result, `submit ${key}`);
        validateSubmissionResponse(response);
      } catch (error) {
        await appendEvent(root, {
          type: "submission_error",
          artifactKey: key,
          attemptId,
          observedAt: new Date().toISOString(),
          message: boundedMessage(error),
        });
        throw new Error(
          `Apple submission outcome is uncertain for ${key}; run the status command to reconcile history before resubmitting`,
          { cause: error },
        );
      }
      const responsePath = join(
        NOTARY_DIRECTORY,
        "responses",
        `${attemptId}-submit.json`,
      );
      await writeJsonAtomic(root, responsePath, response);
      await appendEvent(root, {
        type: "submission_recorded",
        artifactKey: key,
        attemptId,
        submissionId: response.id,
        status: response.status,
        submittedAt: response.createdDate ?? startedAt,
        responsePath,
      });
    }
    return projectLedger(root);
  });
}

export async function refreshNotarizationStatus(root, options = {}) {
  return mutateLedger(root, async (state) => {
    requireRelease(state);
    const runner = options.runner ?? runNotaryTool;
    state = await recoverUncertainAttempts(root, state, runner);
    const errors = [];
    for (const [key, artifact] of state.artifacts) {
      const attempt = artifact.activeAttempt;
      if (!attempt || TERMINAL_STATUSES.has(attempt.status)) continue;
      const result = await runner([
        "info",
        attempt.submissionId,
        "--keychain-profile",
        state.release.profile,
        "--output-format",
        "json",
      ]);
      try {
        const response = parseNotaryResponse(result, `info ${key}`);
        validateSubmissionResponse(response, attempt.submissionId);
        const responsePath = join(
          NOTARY_DIRECTORY,
          "responses",
          `${attempt.attemptId}-${randomUUID()}-info.json`,
        );
        await writeJsonAtomic(root, responsePath, response);
        await appendEvent(root, {
          type: "status_observed",
          artifactKey: key,
          attemptId: attempt.attemptId,
          submissionId: attempt.submissionId,
          status: response.status,
          observedAt: new Date().toISOString(),
          responsePath,
        });
      } catch (error) {
        const message = boundedMessage(error);
        await appendEvent(root, {
          type: "status_error",
          artifactKey: key,
          attemptId: attempt.attemptId,
          submissionId: attempt.submissionId,
          observedAt: new Date().toISOString(),
          message,
        });
        errors.push(`${key}: ${message}`);
      }
    }
    state = await projectLedger(root);
    if (errors.length > 0) {
      throw new Error(`notarization status could not be confirmed:\n${errors.join("\n")}`);
    }
    return state;
  });
}

export async function resubmitArtifact(root, key, options = {}) {
  return mutateLedger(root, async (state) => {
    requireRelease(state);
    const artifact = requireArtifact(state, key);
    if (artifact.activeAttempt?.status === "Accepted") {
      throw new Error(`accepted notarization cannot be replaced: ${key}`);
    }
    if (artifact.activeAttempt) {
      await appendEvent(root, {
        type: "submission_orphaned",
        artifactKey: key,
        attemptId: artifact.activeAttempt.attemptId,
        submissionId: artifact.activeAttempt.submissionId ?? null,
        orphanedAt: new Date().toISOString(),
        reason: options.reason ?? "explicit maintainer resubmission",
      });
    }
    return projectLedger(root);
  }).then(() => submitRegisteredArtifacts(root, { ...options, keys: [key] }));
}

export async function assertAccepted(root) {
  const state = await projectLedger(root);
  requireRelease(state);
  await assertSubmissionBytes(root, state);
  for (const [key, artifact] of state.artifacts) {
    await assertConfinedPath(
      root,
      resolveOwnedPath(root, artifact.registration.staplePath),
      true,
    );
    if (artifact.activeAttempt?.status !== "Accepted") {
      throw new Error(`notarization is not accepted for ${key}`);
    }
  }
  if (state.artifacts.size === 0) throw new Error("notarization ledger has no artifacts");
  return state;
}

export async function recordArtifactFinalized(root, key, finalPath) {
  return mutateLedger(root, async (state) => {
    const artifact = requireArtifact(state, key);
    if (artifact.activeAttempt?.status !== "Accepted") {
      throw new Error(`cannot finalize an unaccepted notarization artifact: ${key}`);
    }
    const normalized = normalizeOwnedPath(root, finalPath);
    await assertConfinedPath(root, normalized.absolute);
    const digest = await sha256Path(normalized.absolute);
    if (artifact.finalized) {
      if (
        artifact.finalized.finalPath !== normalized.relative
        || artifact.finalized.sha256 !== digest
      ) {
        throw new Error(`finalized notarization artifact changed: ${key}`);
      }
      return state;
    }
    await appendEvent(root, {
      type: "artifact_finalized",
      artifactKey: key,
      attemptId: artifact.activeAttempt.attemptId,
      submissionId: artifact.activeAttempt.submissionId,
      finalPath: normalized.relative,
      sha256: digest,
      finalizedAt: new Date().toISOString(),
    });
    return projectLedger(root);
  });
}

export async function completeRelease(root, assetPaths) {
  return mutateLedger(root, async (state) => {
    requireRelease(state);
    for (const [key, artifact] of state.artifacts) {
      if (!artifact.finalized) throw new Error(`notarization artifact is not finalized: ${key}`);
    }
    const assets = [];
    for (const path of assetPaths) {
      const normalized = normalizeOwnedPath(root, path);
      await assertConfinedPath(root, normalized.absolute);
      const fileStat = await lstat(normalized.absolute);
      if (!fileStat.isFile() || fileStat.size === 0) {
        throw new Error(`release asset must be a non-empty regular file: ${path}`);
      }
      assets.push({
        name: basename(normalized.absolute),
        path: normalized.relative,
        bytes: fileStat.size,
        sha256: await sha256File(normalized.absolute),
      });
    }
    assets.sort((left, right) => left.name.localeCompare(right.name));
    if (new Set(assets.map((asset) => asset.name)).size !== assets.length) {
      throw new Error("release asset names must be unique");
    }
    if (state.finalizedRelease) {
      if (JSON.stringify(state.finalizedRelease.assets) !== JSON.stringify(assets)) {
        throw new Error("finalized release assets changed");
      }
      return state;
    }
    await appendEvent(root, {
      type: "release_finalized",
      finalizedAt: new Date().toISOString(),
      assets,
    });
    return projectLedger(root);
  });
}

export async function verifyFinalizedRelease(root) {
  const state = await projectLedger(root);
  requireRelease(state);
  if (!state.finalizedRelease) throw new Error("Desktop release is not finalized");
  await assertSubmissionBytes(root, state);
  for (const [key, artifact] of state.artifacts) {
    if (!artifact.finalized) throw new Error(`notarization artifact is not finalized: ${key}`);
    const finalPath = resolveOwnedPath(root, artifact.finalized.finalPath);
    await assertConfinedPath(root, finalPath);
    const actual = await sha256Path(finalPath);
    if (actual !== artifact.finalized.sha256) {
      throw new Error(`finalized notarization artifact checksum mismatch: ${key}`);
    }
  }
  for (const asset of state.finalizedRelease.assets) {
    const path = resolveOwnedPath(root, asset.path);
    await assertConfinedPath(root, path);
    const fileStat = await lstat(path);
    if (fileStat.size !== asset.bytes || await sha256File(path) !== asset.sha256) {
      throw new Error(`finalized release asset checksum mismatch: ${asset.name}`);
    }
  }
  return state;
}

export async function projectLedger(root) {
  const ledgerPath = join(resolve(root), NOTARY_DIRECTORY, LEDGER_NAME);
  let ledgerBytes;
  try {
    ledgerBytes = await readFile(ledgerPath);
  } catch (error) {
    if (error?.code === "ENOENT") return emptyState();
    throw error;
  }
  const content = ledgerBytes.toString("utf8");
  const state = emptyState();
  const lines = content.split("\n");
  let byteOffset = 0;
  for (let index = 0; index < lines.length; index += 1) {
    const lineBytes = Buffer.byteLength(lines[index], "utf8");
    if (lines[index] === "") {
      byteOffset += index < lines.length - 1 ? 1 : 0;
      continue;
    }
    let envelope;
    try {
      envelope = JSON.parse(lines[index]);
    } catch (error) {
      if (index === lines.length - 1 && !content.endsWith("\n")) {
        state.tornTail = {
          validBytes: byteOffset,
          bytes: ledgerBytes.subarray(byteOffset),
        };
        break;
      }
      throw new Error(`invalid notarization ledger JSON at line ${index + 1}`, { cause: error });
    }
    if (
      envelope.schemaVersion !== LEDGER_SCHEMA_VERSION
      || envelope.kind !== LEDGER_KIND
      || !UUID_PATTERN.test(envelope.eventId ?? "")
      || typeof envelope.recordedAt !== "string"
      || typeof envelope.event?.type !== "string"
    ) {
      throw new Error(`invalid notarization ledger event at line ${index + 1}`);
    }
    applyEvent(state, envelope.event);
    state.events.push(envelope);
    byteOffset += lineBytes + (index < lines.length - 1 ? 1 : 0);
  }
  return state;
}

export function stateSummary(state) {
  return {
    release: state.release,
    finalized: Boolean(state.finalizedRelease),
    recoveryRequired: Boolean(state.tornTail),
    artifacts: [...state.artifacts.values()].map((artifact) => ({
      key: artifact.registration.key,
      kind: artifact.registration.kind,
      target: artifact.registration.target,
      submissionId: artifact.activeAttempt?.submissionId ?? null,
      status: artifact.activeAttempt?.status ?? (artifact.activeAttempt ? "Uncertain" : "Not Submitted"),
      statusError: artifact.activeAttempt?.lastError ?? null,
      finalized: Boolean(artifact.finalized),
    })),
  };
}

async function recoverUncertainAttempts(root, state, runner) {
  let uncertain = [...state.artifacts.values()].filter(
    (artifact) => artifact.activeAttempt && !artifact.activeAttempt.submissionId,
  );
  if (uncertain.length === 0) return state;

  for (const artifact of uncertain) {
    const attempt = artifact.activeAttempt;
    const responsePath = join(
      NOTARY_DIRECTORY,
      "responses",
      `${attempt.attemptId}-submit.json`,
    );
    try {
      const response = JSON.parse(await readFile(resolveOwnedPath(root, responsePath), "utf8"));
      validateSubmissionResponse(response);
      await appendEvent(root, {
        type: "submission_recovered",
        artifactKey: artifact.registration.key,
        attemptId: attempt.attemptId,
        submissionId: response.id,
        status: response.status,
        submittedAt: response.createdDate ?? attempt.startedAt,
        recoveredAt: new Date().toISOString(),
        responsePath,
      });
    } catch (error) {
      if (error?.code !== "ENOENT") {
        throw new Error(`invalid atomic submit response for uncertain attempt ${attempt.attemptId}`, { cause: error });
      }
    }
  }
  state = await projectLedger(root);
  uncertain = [...state.artifacts.values()].filter(
    (artifact) => artifact.activeAttempt && !artifact.activeAttempt.submissionId,
  );
  if (uncertain.length === 0) return state;

  const result = await runner([
    "history",
    "--keychain-profile",
    state.release.profile,
    "--output-format",
    "json",
  ]);
  const history = historyEntries(parseNotaryResponse(result, "history recovery"));
  for (const artifact of uncertain) {
    const attempt = artifact.activeAttempt;
    const before = new Set(attempt.historyIdsBefore);
    const startedAt = Date.parse(attempt.startedAt) - (5 * 60 * 1000);
    const candidates = history.filter((entry) =>
      UUID_PATTERN.test(entry.id ?? "")
      && !before.has(entry.id)
      && entry.name === artifact.registration.submissionName
      && Number.isFinite(Date.parse(entry.createdDate))
      && Date.parse(entry.createdDate) >= startedAt
    );
    if (candidates.length > 1) {
      throw new Error(`multiple Apple submissions could match uncertain attempt ${attempt.attemptId}`);
    }
    if (candidates.length === 1) {
      const candidate = candidates[0];
      await appendEvent(root, {
        type: "submission_recovered",
        artifactKey: artifact.registration.key,
        attemptId: attempt.attemptId,
        submissionId: candidate.id,
        status: candidate.status,
        submittedAt: candidate.createdDate,
        recoveredAt: new Date().toISOString(),
      });
    }
  }
  state = await projectLedger(root);
  const stillUncertain = [...state.artifacts.values()].filter(
    (artifact) => artifact.activeAttempt && !artifact.activeAttempt.submissionId,
  );
  if (stillUncertain.length > 0) {
    throw new Error(
      `Apple history did not identify ${stillUncertain.length} uncertain submission(s); use explicit resubmit only after confirming they do not exist`,
    );
  }
  return state;
}

async function assertSubmissionBytes(root, state) {
  for (const [key, artifact] of state.artifacts) {
    const path = resolveOwnedPath(root, artifact.registration.submissionPath);
    await assertConfinedPath(root, path);
    const fileStat = await lstat(path);
    if (
      !fileStat.isFile()
      || fileStat.size !== artifact.registration.bytes
      || await sha256File(path) !== artifact.registration.sha256
    ) {
      throw new Error(`immutable notarization submission changed: ${key}`);
    }
  }
}

function applyEvent(state, event) {
  switch (event.type) {
    case "release_created":
      if (state.release) throw new Error("notarization ledger has multiple release identities");
      validateReleaseIdentity(event.release);
      state.release = event.release;
      break;
    case "artifact_registered": {
      requireRelease(state);
      const artifact = event.artifact;
      validateArtifactIdentity(artifact);
      if (state.artifacts.has(artifact.key)) {
        throw new Error(`notarization artifact registered twice: ${artifact.key}`);
      }
      state.artifacts.set(artifact.key, {
        registration: artifact,
        attempts: [],
        activeAttempt: null,
        finalized: null,
      });
      break;
    }
    case "submission_started": {
      const artifact = requireArtifact(state, event.artifactKey);
      if (artifact.activeAttempt) throw new Error(`artifact already has an active attempt: ${event.artifactKey}`);
      const attempt = {
        attemptId: event.attemptId,
        startedAt: event.startedAt,
        historyIdsBefore: event.historyIdsBefore ?? [],
        submissionId: null,
        status: null,
        lastError: null,
        orphaned: false,
      };
      artifact.attempts.push(attempt);
      artifact.activeAttempt = attempt;
      break;
    }
    case "submission_recorded":
    case "submission_recovered": {
      const attempt = requireAttempt(state, event.artifactKey, event.attemptId);
      if (attempt.orphaned) throw new Error(`orphaned attempt cannot be recovered: ${event.attemptId}`);
      if (attempt.submissionId && attempt.submissionId !== event.submissionId) {
        throw new Error(`submission ID changed for attempt: ${event.attemptId}`);
      }
      validateSubmissionResponse({ id: event.submissionId, status: event.status });
      attempt.submissionId = event.submissionId;
      attempt.status = event.status;
      attempt.lastError = null;
      break;
    }
    case "submission_error": {
      const attempt = requireAttempt(state, event.artifactKey, event.attemptId);
      attempt.lastError = event.message;
      break;
    }
    case "status_observed": {
      const attempt = requireAttempt(state, event.artifactKey, event.attemptId);
      if (attempt.submissionId !== event.submissionId) throw new Error("status submission ID mismatch");
      attempt.status = event.status;
      attempt.lastError = null;
      break;
    }
    case "status_error": {
      const attempt = requireAttempt(state, event.artifactKey, event.attemptId);
      if (attempt.submissionId !== event.submissionId) throw new Error("status error submission ID mismatch");
      attempt.lastError = event.message;
      break;
    }
    case "submission_orphaned": {
      const artifact = requireArtifact(state, event.artifactKey);
      const attempt = requireAttempt(state, event.artifactKey, event.attemptId);
      attempt.orphaned = true;
      if (artifact.activeAttempt === attempt) artifact.activeAttempt = null;
      break;
    }
    case "artifact_finalized": {
      const artifact = requireArtifact(state, event.artifactKey);
      const attempt = requireAttempt(state, event.artifactKey, event.attemptId);
      if (attempt.submissionId !== event.submissionId || attempt.status !== "Accepted") {
        throw new Error(`artifact finalized without an accepted active attempt: ${event.artifactKey}`);
      }
      if (artifact.finalized) throw new Error(`artifact finalized twice: ${event.artifactKey}`);
      artifact.finalized = event;
      break;
    }
    case "release_finalized":
      if (state.finalizedRelease) throw new Error("release finalized twice");
      state.finalizedRelease = event;
      break;
    case "ledger_tail_recovered":
      break;
    default:
      throw new Error(`unsupported notarization ledger event: ${event.type}`);
  }
}

async function mutateLedger(root, callback) {
  const releaseRoot = resolve(root);
  await mkdir(join(releaseRoot, NOTARY_DIRECTORY), { recursive: true, mode: 0o700 });
  await chmod(join(releaseRoot, NOTARY_DIRECTORY), 0o700);
  const lockPath = join(releaseRoot, NOTARY_DIRECTORY, "writer.lock");
  const lock = await acquireLock(lockPath);
  try {
    let state = await projectLedger(releaseRoot);
    if (state.tornTail) {
      await repairTornLedgerTail(releaseRoot, state.tornTail);
      state = await projectLedger(releaseRoot);
    }
    return await callback(state);
  } finally {
    await lock.handle.close();
    await releaseLock(lockPath, lock.lockId);
  }
}

async function acquireLock(lockPath) {
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      const handle = await open(lockPath, "wx", 0o600);
      const lockId = randomUUID();
      await handle.writeFile(`${JSON.stringify({
        lockId,
        pid: process.pid,
        host: hostname(),
        createdAt: new Date().toISOString(),
      })}\n`);
      await handle.sync();
      return { handle, lockId };
    } catch (error) {
      if (error?.code !== "EEXIST") throw error;
      const stale = await staleLocalLock(lockPath);
      if (!stale) throw new Error(`another notarization state operation is active: ${lockPath}`);
      await rm(lockPath, { force: true });
    }
  }
  throw new Error(`could not acquire notarization state lock: ${lockPath}`);
}

async function staleLocalLock(lockPath) {
  try {
    const owner = JSON.parse(await readFile(lockPath, "utf8"));
    if (owner.host !== hostname() || !Number.isInteger(owner.pid) || owner.pid < 1) return false;
    if (Date.now() - Date.parse(owner.createdAt) > 2 * 60 * 60 * 1000) return true;
    try {
      process.kill(owner.pid, 0);
      return false;
    } catch (error) {
      return error?.code === "ESRCH";
    }
  } catch {
    try {
      return Date.now() - (await stat(lockPath)).mtimeMs > 60 * 1000;
    } catch {
      return false;
    }
  }
}

async function releaseLock(lockPath, lockId) {
  try {
    const owner = JSON.parse(await readFile(lockPath, "utf8"));
    if (owner.lockId === lockId) await rm(lockPath, { force: true });
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
}

async function appendEvent(root, event) {
  const envelope = {
    schemaVersion: LEDGER_SCHEMA_VERSION,
    kind: LEDGER_KIND,
    eventId: randomUUID(),
    recordedAt: new Date().toISOString(),
    event,
  };
  const line = `${JSON.stringify(envelope)}\n`;
  if (Buffer.byteLength(line) > 64 * 1024) throw new Error("notarization ledger event exceeds 64 KiB");
  const path = join(resolve(root), NOTARY_DIRECTORY, LEDGER_NAME);
  const handle = await open(path, "a", 0o600);
  try {
    await handle.chmod(0o600);
    await handle.write(line);
    await handle.sync();
  } finally {
    await handle.close();
  }
}

async function repairTornLedgerTail(root, tornTail) {
  const recoveryPath = join(
    NOTARY_DIRECTORY,
    "recovery",
    `${randomUUID()}-torn-tail.json`,
  );
  await writeJsonAtomic(root, recoveryPath, {
    recoveredAt: new Date().toISOString(),
    bytes: tornTail.bytes.length,
    sha256: createHash("sha256").update(tornTail.bytes).digest("hex"),
    base64: tornTail.bytes.toString("base64"),
  });
  await truncate(join(resolve(root), NOTARY_DIRECTORY, LEDGER_NAME), tornTail.validBytes);
  await appendEvent(root, {
    type: "ledger_tail_recovered",
    recoveryPath,
    bytes: tornTail.bytes.length,
    recoveredAt: new Date().toISOString(),
  });
}

async function writeJsonAtomic(root, relativePath, value) {
  const finalPath = resolveOwnedPath(root, relativePath);
  await mkdir(dirname(finalPath), { recursive: true, mode: 0o700 });
  const temporaryPath = `${finalPath}.${randomUUID()}.tmp`;
  const handle = await open(temporaryPath, "wx", 0o600);
  try {
    await handle.writeFile(`${JSON.stringify(value, null, 2)}\n`);
    await handle.sync();
  } finally {
    await handle.close();
  }
  await rename(temporaryPath, finalPath);
  const directory = await open(dirname(finalPath), "r");
  try {
    await directory.sync();
  } finally {
    await directory.close();
  }
}

async function runNotaryTool(args) {
  const result = spawnSync("xcrun", ["notarytool", ...args], {
    encoding: "utf8",
    maxBuffer: 4 * 1024 * 1024,
    timeout: 15 * 60 * 1000,
  });
  return {
    status: result.status,
    signal: result.signal,
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? "",
    error: result.error,
  };
}

function parseNotaryResponse(result, operation) {
  let response;
  try {
    response = JSON.parse(result.stdout);
  } catch (error) {
    const detail = boundedText(result.stderr || result.error?.message || result.stdout || "no response");
    throw new Error(`${operation} returned invalid JSON: ${detail}`, { cause: error });
  }
  if (typeof response.message === "string" && !response.status && !response.history) {
    throw new Error(`${operation} failed: ${boundedText(response.message)}`);
  }
  if (result.status !== 0 && !response.id) {
    throw new Error(`${operation} exited with ${result.status ?? result.signal ?? "unknown"}`);
  }
  return response;
}

function validateSubmissionResponse(response, expectedId) {
  if (!UUID_PATTERN.test(response.id ?? "")) throw new Error("Apple response has no valid submission ID");
  if (expectedId && response.id !== expectedId) throw new Error("Apple response submission ID changed");
  if (typeof response.status !== "string" || response.status.length === 0) {
    throw new Error("Apple response has no notarization status");
  }
}

function historyEntries(response) {
  const entries = Array.isArray(response.history) ? response.history : [];
  return entries.filter((entry) => UUID_PATTERN.test(entry?.id ?? ""));
}

function validateReleaseIdentity({ tag, commit, teamId, profile }) {
  if (!/^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(tag ?? "")) {
    throw new Error(`invalid release tag: ${tag}`);
  }
  if (!/^[0-9a-f]{40}$/.test(commit ?? "")) throw new Error(`invalid release commit: ${commit}`);
  if (!/^[A-Z0-9]{10}$/.test(teamId ?? "")) throw new Error(`invalid Apple Team ID: ${teamId}`);
  if (typeof profile !== "string" || profile.length === 0 || profile.length > 128) {
    throw new Error("invalid notarytool keychain profile");
  }
}

function validateArtifactIdentity({ key, kind, target, expectedArch }) {
  if (!/^[a-z0-9][a-z0-9_-]{1,80}$/.test(key ?? "")) throw new Error(`invalid artifact key: ${key}`);
  if (!new Set(["dmg", "app_zip"]).has(kind)) throw new Error(`invalid artifact kind: ${kind}`);
  const mapping = {
    "aarch64-apple-darwin": "arm64",
    "x86_64-apple-darwin": "x86_64",
  };
  if (mapping[target] !== expectedArch) throw new Error(`invalid target architecture mapping: ${target}`);
}

function validateWebhook(value) {
  const url = new URL(value);
  if (url.protocol !== "https:") throw new Error("Apple notarization webhook must use HTTPS");
  if (url.username || url.password || url.hash) throw new Error("Apple notarization webhook URL is invalid");
}

function normalizeOwnedPath(root, path) {
  const releaseRoot = resolve(root);
  const absolute = resolve(releaseRoot, path);
  if (absolute === releaseRoot || !absolute.startsWith(`${releaseRoot}${sep}`)) {
    throw new Error(`path escapes release root: ${path}`);
  }
  return { absolute, relative: relative(releaseRoot, absolute) };
}

function resolveOwnedPath(root, path) {
  return normalizeOwnedPath(root, path).absolute;
}

async function assertConfinedPath(root, path, allowMissingLeaf = false) {
  const releaseRoot = await realpath(resolve(root));
  let candidate;
  try {
    const metadata = await lstat(path);
    if (metadata.isSymbolicLink()) throw new Error(`path must not be a symlink: ${path}`);
    candidate = await realpath(path);
  } catch (error) {
    if (!allowMissingLeaf || error?.code !== "ENOENT") throw error;
    candidate = join(await realpath(dirname(path)), basename(path));
  }
  if (candidate === releaseRoot || !candidate.startsWith(`${releaseRoot}${sep}`)) {
    throw new Error(`path resolves outside release root: ${path}`);
  }
}

function emptyState() {
  return {
    release: null,
    artifacts: new Map(),
    finalizedRelease: null,
    events: [],
    tornTail: null,
  };
}

function requireRelease(state) {
  if (state.tornTail) {
    throw new Error("notarization ledger has a torn tail; run a state-changing command to recover it");
  }
  if (!state.release) throw new Error("notarization ledger is not initialized");
  return state.release;
}

function requireArtifact(state, key) {
  const artifact = state.artifacts.get(key);
  if (!artifact) throw new Error(`unknown notarization artifact: ${key}`);
  return artifact;
}

function requireAttempt(state, key, attemptId) {
  const artifact = requireArtifact(state, key);
  const attempt = artifact.attempts.find((candidate) => candidate.attemptId === attemptId);
  if (!attempt) throw new Error(`unknown notarization attempt: ${attemptId}`);
  return attempt;
}

async function sha256File(path) {
  const digest = createHash("sha256");
  await new Promise((accept, reject) => {
    const stream = createReadStream(path);
    stream.on("data", (chunk) => digest.update(chunk));
    stream.on("error", reject);
    stream.on("end", accept);
  });
  return digest.digest("hex");
}

async function sha256Path(path) {
  const entry = await lstat(path);
  if (entry.isFile()) return sha256File(path);
  if (!entry.isDirectory()) throw new Error(`finalized path must be a file or directory: ${path}`);
  const digest = createHash("sha256");
  const visit = async (directory, prefix = "") => {
    const entries = await readdir(directory, { withFileTypes: true });
    entries.sort((left, right) => left.name.localeCompare(right.name));
    for (const child of entries) {
      const childPath = join(directory, child.name);
      const relativePath = prefix ? `${prefix}/${child.name}` : child.name;
      const metadata = await lstat(childPath);
      if (metadata.isDirectory()) {
        digest.update(`d\0${relativePath}\0${metadata.mode & 0o777}\n`);
        await visit(childPath, relativePath);
      } else if (metadata.isSymbolicLink()) {
        digest.update(`l\0${relativePath}\0${await readlink(childPath)}\n`);
      } else if (metadata.isFile()) {
        digest.update(`f\0${relativePath}\0${metadata.mode & 0o777}\0${metadata.size}\0${await sha256File(childPath)}\n`);
      } else {
        throw new Error(`unsupported finalized bundle entry: ${childPath}`);
      }
    }
  };
  await visit(path);
  return digest.digest("hex");
}

function boundedText(value) {
  return String(value).replaceAll(/[\r\n]+/g, " ").slice(0, 1000);
}

function boundedMessage(error) {
  return boundedText(error instanceof Error ? error.message : error);
}

function printSummary(state) {
  process.stdout.write(`${JSON.stringify(stateSummary(state), null, 2)}\n`);
}

async function run() {
  const [command, ...args] = process.argv.slice(2);
  if (command === "init" && args.length === 5) {
    printSummary(await initializeLedger({
      root: args[0], tag: args[1], commit: args[2], teamId: args[3], profile: args[4],
    }));
    return;
  }
  if (command === "register" && args.length === 7) {
    printSummary(await registerArtifact({
      root: args[0], key: args[1], kind: args[2], target: args[3], expectedArch: args[4],
      submissionPath: args[5], staplePath: args[6],
    }));
    return;
  }
  if (command === "submit" && (args.length === 1 || args.length === 2)) {
    printSummary(await submitRegisteredArtifacts(args[0], { webhook: args[1] }));
    return;
  }
  if (command === "status" && args.length === 1) {
    printSummary(await refreshNotarizationStatus(args[0]));
    return;
  }
  if (command === "summary" && args.length === 1) {
    printSummary(await projectLedger(args[0]));
    return;
  }
  if (command === "assert-accepted" && args.length === 1) {
    printSummary(await assertAccepted(args[0]));
    return;
  }
  if (command === "resubmit" && args.length === 2) {
    printSummary(await resubmitArtifact(args[0], args[1]));
    return;
  }
  if (command === "record-finalized" && args.length === 3) {
    printSummary(await recordArtifactFinalized(args[0], args[1], args[2]));
    return;
  }
  if (command === "complete" && args.length >= 2) {
    printSummary(await completeRelease(args[0], args.slice(1)));
    return;
  }
  if (command === "verify-finalized" && args.length === 1) {
    printSummary(await verifyFinalizedRelease(args[0]));
    return;
  }
  if (command === "get-release" && args.length === 2) {
    const state = await projectLedger(args[0]);
    const value = requireRelease(state)[args[1]];
    if (typeof value !== "string") throw new Error(`unsupported release field: ${args[1]}`);
    process.stdout.write(value);
    return;
  }
  if (command === "list-targets" && args.length === 1) {
    const state = await projectLedger(args[0]);
    requireRelease(state);
    const targets = [...new Set(
      [...state.artifacts.values()].map((artifact) => artifact.registration.target),
    )].sort();
    process.stdout.write(`${targets.join("\n")}${targets.length > 0 ? "\n" : ""}`);
    return;
  }
  throw new Error(
    "Usage: desktop-notarization-state.mjs init|register|submit|status|summary|assert-accepted|resubmit|record-finalized|complete|verify-finalized|get-release|list-targets ...",
  );
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  run().catch((error) => {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  });
}
