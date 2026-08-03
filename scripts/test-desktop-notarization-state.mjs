#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  appendFile,
  chmod,
  mkdtemp,
  mkdir,
  readFile,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  assertAccepted,
  completeRelease,
  initializeLedger,
  projectLedger,
  recordArtifactFinalized,
  refreshNotarizationStatus,
  registerArtifact,
  resubmitArtifact,
  stateSummary,
  submitRegisteredArtifacts,
  verifyFinalizedRelease,
} from "./desktop-notarization-state.mjs";

const TAG = "v1.2.3-beta.4";
const COMMIT = "a".repeat(40);
const TEAM_ID = "AB12CD34EF";
const PROFILE = "Sigil-Notary-Test";
const FIRST_ID = "11111111-1111-4111-8111-111111111111";
const SECOND_ID = "22222222-2222-4222-8222-222222222222";

await testAcceptedLifecycle();
await testUncertainSubmissionRecovery();
await testAtomicResponseRecovery();
await testMissingSubmissionRequiresExplicitResubmit();
await testImmutableSubmissionAndFinalAssetBinding();
await testTornLedgerTailRecovery();
await testCliWithFakeNotaryTool();
await testSubmissionSymlinkEscapeIsRejected();
await testDualArchitectureSubmissionMatrix();

console.log("desktop notarization state: ok");

async function testAcceptedLifecycle() {
  const fixture = await createFixture();
  const responses = [
    history([]),
    success({ id: FIRST_ID, status: "In Progress", createdDate: "2026-08-03T00:00:00Z" }),
    success({ id: FIRST_ID, status: "Accepted", createdDate: "2026-08-03T00:00:00Z" }),
  ];
  const runner = async () => responses.shift();
  await submitRegisteredArtifacts(fixture.root, { runner });
  let summary = stateSummary(await projectLedger(fixture.root));
  assert.equal(summary.artifacts[0].status, "In Progress");
  await refreshNotarizationStatus(fixture.root, { runner });
  await assertAccepted(fixture.root);
  await refreshNotarizationStatus(fixture.root, {
    runner: async () => {
      throw new Error("terminal Accepted status must not be queried again");
    },
  });
  await assert.rejects(
    resubmitArtifact(fixture.root, "arm64_dmg", { runner }),
    /accepted notarization cannot be replaced/,
  );

  await writeFile(fixture.finalPath, "stapled-dmg");
  await recordArtifactFinalized(fixture.root, "arm64_dmg", fixture.finalPath);
  await writeFile(fixture.releaseAsset, "public-asset");
  await completeRelease(fixture.root, [fixture.releaseAsset]);
  await verifyFinalizedRelease(fixture.root);

  summary = stateSummary(await projectLedger(fixture.root));
  assert.equal(summary.finalized, true);
  assert.equal(summary.artifacts[0].finalized, true);
  const ledger = await readFile(join(fixture.root, ".notary", "events.jsonl"), "utf8");
  assert.ok(ledger.length > 0);
  assert.ok(ledger.endsWith("\n"));
  const submitResponses = await readFile(
    join(
      fixture.root,
      (await projectLedger(fixture.root)).events.find(
        ({ event }) => event.type === "submission_recorded",
      ).event.responsePath,
    ),
    "utf8",
  );
  assert.equal(JSON.parse(submitResponses).id, FIRST_ID);
}

async function testUncertainSubmissionRecovery() {
  const fixture = await createFixture();
  const failedSubmit = async (args) => {
    if (args[0] === "history") return history([]);
    return { status: 139, stdout: "", stderr: "Bus error" };
  };
  await assert.rejects(
    submitRegisteredArtifacts(fixture.root, { runner: failedSubmit }),
    /outcome is uncertain/,
  );
  let state = await projectLedger(fixture.root);
  assert.equal(state.artifacts.get("arm64_dmg").activeAttempt.submissionId, null);

  const recoveredRunner = async (args) => {
    if (args[0] === "history") {
      return history([{ id: FIRST_ID, name: "submission.dmg", status: "Accepted", createdDate: new Date().toISOString() }]);
    }
    throw new Error(`unexpected command: ${args[0]}`);
  };
  await refreshNotarizationStatus(fixture.root, { runner: recoveredRunner });
  state = await assertAccepted(fixture.root);
  assert.equal(state.artifacts.get("arm64_dmg").activeAttempt.submissionId, FIRST_ID);
}

async function testMissingSubmissionRequiresExplicitResubmit() {
  const fixture = await createFixture();
  const initial = [
    history([]),
    success({ id: FIRST_ID, status: "In Progress", createdDate: new Date().toISOString() }),
  ];
  await submitRegisteredArtifacts(fixture.root, { runner: async () => initial.shift() });
  const missing = async (args) => {
    if (args[0] === "info") {
      return {
        status: 1,
        stdout: JSON.stringify({
          id: FIRST_ID,
          message: "Submission does not exist or does not belong to your team.",
        }),
        stderr: "",
      };
    }
    throw new Error(`unexpected command: ${args[0]}`);
  };
  await assert.rejects(refreshNotarizationStatus(fixture.root, { runner: missing }), /does not belong/);
  let state = await projectLedger(fixture.root);
  assert.equal(state.artifacts.get("arm64_dmg").activeAttempt.submissionId, FIRST_ID);
  assert.match(state.artifacts.get("arm64_dmg").activeAttempt.lastError, /does not belong/);

  const replacement = [
    history([{ id: FIRST_ID, name: "submission.dmg", status: "In Progress", createdDate: new Date().toISOString() }]),
    success({ id: SECOND_ID, status: "Accepted", createdDate: new Date().toISOString() }),
  ];
  await resubmitArtifact(fixture.root, "arm64_dmg", { runner: async () => replacement.shift() });
  state = await assertAccepted(fixture.root);
  const artifact = state.artifacts.get("arm64_dmg");
  assert.equal(artifact.attempts.length, 2);
  assert.equal(artifact.attempts[0].orphaned, true);
  assert.equal(artifact.activeAttempt.submissionId, SECOND_ID);
}

async function testAtomicResponseRecovery() {
  const fixture = await createFixture();
  const failedSubmit = async (args) => {
    if (args[0] === "history") return history([]);
    return { status: 139, stdout: "", stderr: "Bus error" };
  };
  await assert.rejects(
    submitRegisteredArtifacts(fixture.root, { runner: failedSubmit }),
    /outcome is uncertain/,
  );
  const state = await projectLedger(fixture.root);
  const attemptId = state.artifacts.get("arm64_dmg").activeAttempt.attemptId;
  const responseDirectory = join(fixture.root, ".notary", "responses");
  await mkdir(responseDirectory, { recursive: true });
  await writeFile(
    join(responseDirectory, `${attemptId}-submit.json`),
    JSON.stringify({ id: FIRST_ID, status: "Accepted", createdDate: new Date().toISOString() }),
  );
  await refreshNotarizationStatus(fixture.root, {
    runner: async () => {
      throw new Error("history should not be needed after atomic response recovery");
    },
  });
  assert.equal(
    (await assertAccepted(fixture.root)).artifacts.get("arm64_dmg").activeAttempt.submissionId,
    FIRST_ID,
  );
}

async function testImmutableSubmissionAndFinalAssetBinding() {
  const fixture = await createFixture();
  const responses = [
    history([]),
    success({ id: FIRST_ID, status: "Accepted", createdDate: new Date().toISOString() }),
  ];
  await submitRegisteredArtifacts(fixture.root, { runner: async () => responses.shift() });
  await writeFile(fixture.submissionPath, "tampered");
  await assert.rejects(assertAccepted(fixture.root), /immutable notarization submission changed/);

  await writeFile(fixture.submissionPath, "raw-dmg");
  await writeFile(fixture.finalPath, "stapled-dmg");
  await recordArtifactFinalized(fixture.root, "arm64_dmg", fixture.finalPath);
  await writeFile(fixture.releaseAsset, "public-asset");
  await completeRelease(fixture.root, [fixture.releaseAsset]);
  await writeFile(fixture.releaseAsset, "changed-after-finalization");
  await assert.rejects(verifyFinalizedRelease(fixture.root), /checksum mismatch/);
}

async function testTornLedgerTailRecovery() {
  const fixture = await createFixture();
  const ledgerPath = join(fixture.root, ".notary", "events.jsonl");
  await appendFile(ledgerPath, '{"schemaVersion":1,"kind":"sigil-desktop');
  let state = await projectLedger(fixture.root);
  assert.ok(state.tornTail);
  await initializeLedger({
    root: fixture.root,
    tag: TAG,
    commit: COMMIT,
    teamId: TEAM_ID,
    profile: PROFILE,
  });
  state = await projectLedger(fixture.root);
  assert.equal(state.tornTail, null);
  assert.ok(state.events.some(({ event }) => event.type === "ledger_tail_recovered"));
}

async function testCliWithFakeNotaryTool() {
  const fixture = await createFixture();
  const binaryDirectory = join(fixture.root, "fake-bin");
  await mkdir(binaryDirectory);
  const fakeXcrun = join(binaryDirectory, "xcrun");
  await writeFile(fakeXcrun, `#!/bin/sh
case "$2" in
  history) printf '%s\\n' '{"history":[]}' ;;
  submit) printf '%s\\n' '{"id":"${FIRST_ID}","status":"Accepted","createdDate":"2026-08-03T00:00:00Z"}' ;;
  *) printf '%s\\n' '{"message":"unexpected fake command"}'; exit 1 ;;
esac
`);
  await chmod(fakeXcrun, 0o755);
  const environment = { ...process.env, PATH: `${binaryDirectory}:${process.env.PATH}` };
  const submit = spawnSync(
    process.execPath,
    [join(import.meta.dirname, "desktop-notarization-state.mjs"), "submit", fixture.root],
    { encoding: "utf8", env: environment },
  );
  assert.equal(submit.status, 0, submit.stderr);
  assert.match(submit.stdout, /Accepted/);

  const summary = spawnSync(
    join(import.meta.dirname, "status-desktop-macos-notarization.sh"),
    ["--artifact-dir", fixture.root, "--summary"],
    { encoding: "utf8", env: environment },
  );
  assert.equal(summary.status, 0, summary.stderr);
  assert.equal(JSON.parse(summary.stdout).artifacts[0].submissionId, FIRST_ID);
}

async function testSubmissionSymlinkEscapeIsRejected() {
  const root = await mkdtemp(join(tmpdir(), "sigil-notary-symlink-"));
  const outsideRoot = await mkdtemp(join(tmpdir(), "sigil-notary-outside-"));
  const outsideDmg = join(outsideRoot, "outside.dmg");
  const linkedDmg = join(root, "linked.dmg");
  await writeFile(outsideDmg, "outside");
  await symlink(outsideDmg, linkedDmg);
  await initializeLedger({ root, tag: TAG, commit: COMMIT, teamId: TEAM_ID, profile: PROFILE });
  await assert.rejects(registerArtifact({
    root,
    key: "arm64_dmg",
    kind: "dmg",
    target: "aarch64-apple-darwin",
    expectedArch: "arm64",
    submissionPath: linkedDmg,
    staplePath: join(root, "final.dmg"),
  }), /must not be a symlink/);
}

async function testDualArchitectureSubmissionMatrix() {
  const root = await mkdtemp(join(tmpdir(), "sigil-notary-matrix-"));
  await initializeLedger({ root, tag: TAG, commit: COMMIT, teamId: TEAM_ID, profile: PROFILE });
  const specifications = [
    ["arm64_dmg", "dmg", "aarch64-apple-darwin", "arm64"],
    ["arm64_app", "app_zip", "aarch64-apple-darwin", "arm64"],
    ["x86_64_dmg", "dmg", "x86_64-apple-darwin", "x86_64"],
    ["x86_64_app", "app_zip", "x86_64-apple-darwin", "x86_64"],
  ];
  for (const [key, kind, target, expectedArch] of specifications) {
    const submissionPath = join(root, `${key}.zip`);
    const staplePath = join(root, `${key}.final`);
    await writeFile(submissionPath, key);
    await registerArtifact({
      root,
      key,
      kind,
      target,
      expectedArch,
      submissionPath,
      staplePath,
    });
  }
  const submissionIds = [
    "33333333-3333-4333-8333-333333333333",
    "44444444-4444-4444-8444-444444444444",
    "55555555-5555-4555-8555-555555555555",
    "66666666-6666-4666-8666-666666666666",
  ];
  const calls = [];
  const runner = async (args) => {
    calls.push(args);
    if (args[0] === "history") return history([]);
    if (args[0] === "submit") {
      return success({
        id: submissionIds.shift(),
        status: "Accepted",
        createdDate: new Date().toISOString(),
      });
    }
    throw new Error(`unexpected command: ${args[0]}`);
  };
  const state = await submitRegisteredArtifacts(root, {
    runner,
    webhook: "https://relay.example/apple-notary/secret",
  });
  assert.equal(stateSummary(state).artifacts.length, 4);
  assert.ok(stateSummary(state).artifacts.every((artifact) => artifact.status === "Accepted"));
  assert.equal(calls.filter(([command]) => command === "history").length, 4);
  assert.equal(calls.filter(([command]) => command === "submit").length, 4);
  assert.ok(
    calls.filter(([command]) => command === "submit")
      .every((args) => args.includes("--webhook")),
  );
  await assertAccepted(root);
}

async function createFixture() {
  const root = await mkdtemp(join(tmpdir(), "sigil-notary-state-"));
  await mkdir(join(root, ".work", "aarch64-apple-darwin"), { recursive: true });
  const submissionPath = join(root, ".work", "aarch64-apple-darwin", "submission.dmg");
  const finalPath = join(root, "Sigil_1.2.3-beta.4_aarch64-apple-darwin.dmg");
  const releaseAsset = join(root, "Sigil_1.2.3-beta.4_aarch64-apple-darwin.dmg.sha256");
  await writeFile(submissionPath, "raw-dmg");
  await initializeLedger({ root, tag: TAG, commit: COMMIT, teamId: TEAM_ID, profile: PROFILE });
  await registerArtifact({
    root,
    key: "arm64_dmg",
    kind: "dmg",
    target: "aarch64-apple-darwin",
    expectedArch: "arm64",
    submissionPath,
    staplePath: finalPath,
  });
  return { root, submissionPath, finalPath, releaseAsset };
}

function success(value) {
  return { status: 0, stdout: JSON.stringify(value), stderr: "" };
}

function history(entries) {
  return success({ history: entries });
}
