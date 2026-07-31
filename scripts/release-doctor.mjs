#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import {
  lstatSync,
  readFileSync,
} from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { parseReleaseVersion } from "./release-channel-version.mjs";

const DEFAULT_REQUIRED_WORKFLOW = "CI";

function usage() {
  return `Usage: scripts/release-doctor.mjs --tag v<version> [options]

Validate that one release tag agrees with Cargo, Desktop, Tauri, Cargo.lock,
and both public changelogs. Optional release-owner checks bind the candidate to
a clean main checkout, the remote repository, successful exact-SHA workflows,
and the publisher's updater public key.

Options:
  --root PATH                  Repository root. Defaults to the current checkout.
  --repository OWNER/REPO     GitHub repository. Inferred when possible.
  --require-clean             Refuse tracked or untracked worktree changes.
  --require-head-tag          Require HEAD to equal the local tag commit.
  --require-origin-main       Require main and the remote main ref to equal HEAD.
  --require-remote-tag        Require the remote tag commit to equal the local tag.
  --require-ci                Require successful exact-HEAD GitHub workflow runs.
  --require-workflow NAME     Required workflow name; repeatable. Defaults to CI.
  --wait-ci-seconds SECONDS   Wait for missing/in-progress required runs.
  --require-public-channel    Require install surfaces to name this release channel/tag.
  --updater-public-key PATH   Require this public key to match Tauri's embedded key.
  -h, --help                  Show this help.`;
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function run(command, args, cwd) {
  const result = spawnSync(command, args, {
    cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.error !== undefined) throw result.error;
  if (result.status !== 0) {
    const detail = result.stderr.trim() || result.stdout.trim();
    throw new Error(`${command} ${args.join(" ")} failed${detail ? `: ${detail}` : ""}`);
  }
  return result.stdout.trim();
}

function extractWorkspaceVersion(manifest) {
  const match = /^\[workspace\.package\][\s\S]*?^version = "([^"]+)"/m.exec(manifest);
  if (match === null) throw new Error("Cargo.toml has no workspace package version");
  return match[1];
}

function extractSigilLockVersions(lockfile) {
  const versions = new Map();
  for (const block of lockfile.split(/\n(?=\[\[package\]\]\n)/)) {
    const name = /^name = "(sigil(?:-[^"]+)?)"$/m.exec(block)?.[1];
    const version = /^version = "([^"]+)"$/m.exec(block)?.[1];
    if (name !== undefined && version !== undefined) versions.set(name, version);
  }
  return versions;
}

export function inspectReleaseSource({ root, tag, updaterPublicKey }) {
  if (!/^v[^\s]+$/.test(tag)) throw new Error(`release tag must be v-prefixed: ${tag}`);
  const version = tag.slice(1);
  parseReleaseVersion(version);

  const cargoVersion = extractWorkspaceVersion(
    readFileSync(resolve(root, "Cargo.toml"), "utf8"),
  );
  const desktopVersion = readJson(resolve(root, "apps/desktop/package.json")).version;
  const tauriConfig = readJson(resolve(root, "apps/desktop/src-tauri/tauri.conf.json"));
  const tauriVersion = tauriConfig.version;
  const observed = new Map([
    ["Cargo workspace", cargoVersion],
    ["Desktop package", desktopVersion],
    ["Tauri bundle", tauriVersion],
  ]);
  for (const [surface, observedVersion] of observed) {
    if (observedVersion !== version) {
      throw new Error(`${surface} version ${observedVersion ?? "<missing>"} does not match ${tag}`);
    }
  }

  const lockVersions = extractSigilLockVersions(
    readFileSync(resolve(root, "Cargo.lock"), "utf8"),
  );
  for (const requiredPackage of ["sigil", "sigil-desktop-app", "sigil-runtime", "sigil-tui"]) {
    if (lockVersions.get(requiredPackage) !== version) {
      throw new Error(
        `Cargo.lock ${requiredPackage} version ${lockVersions.get(requiredPackage) ?? "<missing>"} does not match ${tag}`,
      );
    }
  }
  for (const [packageName, packageVersion] of lockVersions) {
    if (packageVersion !== version) {
      throw new Error(`Cargo.lock ${packageName} version ${packageVersion} does not match ${tag}`);
    }
  }

  for (const changelog of ["docs/en/changelog.md", "docs/zh-CN/changelog.md"]) {
    const contents = readFileSync(resolve(root, changelog), "utf8");
    if (!contents.includes(`## ${tag} - `)) {
      throw new Error(`${changelog} has no exact ${tag} release heading`);
    }
  }

  if (updaterPublicKey !== undefined) {
    const keyPath = resolve(updaterPublicKey);
    const metadata = lstatSync(keyPath);
    if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size === 0) {
      throw new Error(`updater public key must be a non-empty regular file: ${keyPath}`);
    }
    const publisherKey = readFileSync(keyPath, "utf8").trim();
    if (tauriConfig.plugins?.updater?.pubkey !== publisherKey) {
      throw new Error("publisher updater public key does not match the embedded Tauri key");
    }
  }

  return { tag, version, lockPackages: [...lockVersions.keys()].sort() };
}

function inspectPublicChannel(root, tag, version) {
  const channel = parseReleaseVersion(version).channel;
  const npmTag = channel === "stable" ? "latest" : channel;
  const installCommand = `npm install -g @sigil-ai/sigil@${npmTag}`;
  for (const path of [
    "README.md",
    "README.zh-CN.md",
    "docs/en/quickstart.md",
    "docs/zh-CN/quickstart.md",
    "site/index.html",
    "site/zh-CN/index.html",
  ]) {
    if (!readFileSync(resolve(root, path), "utf8").includes(installCommand)) {
      throw new Error(`${path} does not advertise candidate npm channel ${npmTag}`);
    }
  }
  for (const path of ["docs/en/installation.md", "docs/zh-CN/installation.md"]) {
    const contents = readFileSync(resolve(root, path), "utf8");
    if (!contents.includes(installCommand) || !contents.includes(`--tag ${tag} --locked sigil`)) {
      throw new Error(`${path} does not bind install guidance to ${tag} and npm ${npmTag}`);
    }
  }
}

function inferRepository(root) {
  const remote = run("git", ["remote", "get-url", "origin"], root);
  const match = /(?:github\.com[:/])([^/\s]+\/[^/\s]+?)(?:\.git)?$/.exec(remote);
  if (match === null) throw new Error("cannot infer OWNER/REPO from origin");
  return match[1];
}

function exactShaWorkflowState({ root, repository, commit, workflows }) {
  const runs = JSON.parse(
    run(
      "gh",
      [
        "run",
        "list",
        "--repo",
        repository,
        "--commit",
        commit,
        "--limit",
        "100",
        "--json",
        "workflowName,status,conclusion,headSha,url",
      ],
      root,
    ),
  );
  const pending = [];
  for (const workflow of workflows) {
    const matching = runs.filter(
      (candidate) => candidate.workflowName === workflow && candidate.headSha === commit,
    );
    const success = matching.find(
      (candidate) => candidate.status === "completed" && candidate.conclusion === "success",
    );
    if (success === undefined) {
      const evidence = matching.map((candidate) => `${candidate.status}/${candidate.conclusion}`).join(", ");
      if (matching.length === 0 || matching.some((candidate) => candidate.status !== "completed")) {
        pending.push(`${workflow}${evidence ? ` (${evidence})` : " (not started)"}`);
        continue;
      }
      throw new Error(`exact commit ${commit} has no successful ${workflow} run (${evidence})`);
    }
  }
  return pending;
}

function requireExactShaWorkflows(options, waitSeconds) {
  const deadline = Date.now() + waitSeconds * 1000;
  while (true) {
    const pending = exactShaWorkflowState(options);
    if (pending.length === 0) return;
    if (Date.now() >= deadline) {
      throw new Error(
        `exact commit ${options.commit} workflows did not become successful: ${pending.join(", ")}`,
      );
    }
    console.log(`waiting for exact-SHA workflows: ${pending.join(", ")}`);
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 30_000);
  }
}

function parseArgs(args) {
  const options = {
    root: process.cwd(),
    workflows: [],
    requireClean: false,
    requireHeadTag: false,
    requireOriginMain: false,
    requireRemoteTag: false,
    requireCi: false,
    requirePublicChannel: false,
    waitCiSeconds: 0,
  };
  for (let index = 0; index < args.length; index += 1) {
    const flag = args[index];
    if (flag === "-h" || flag === "--help") return { help: true };
    if ([
      "--require-clean",
      "--require-head-tag",
      "--require-origin-main",
      "--require-remote-tag",
      "--require-ci",
      "--require-public-channel",
    ].includes(flag)) {
      const property = {
        "--require-clean": "requireClean",
        "--require-head-tag": "requireHeadTag",
        "--require-origin-main": "requireOriginMain",
        "--require-remote-tag": "requireRemoteTag",
        "--require-ci": "requireCi",
        "--require-public-channel": "requirePublicChannel",
      }[flag];
      options[property] = true;
      continue;
    }
    const value = args[index + 1];
    if (value === undefined) throw new Error(`missing value for ${flag}`);
    index += 1;
    switch (flag) {
      case "--tag": options.tag = value; break;
      case "--root": options.root = value; break;
      case "--repository": options.repository = value; break;
      case "--require-workflow": options.workflows.push(value); break;
      case "--wait-ci-seconds": options.waitCiSeconds = Number(value); break;
      case "--updater-public-key": options.updaterPublicKey = value; break;
      default: throw new Error(`unknown argument: ${flag}`);
    }
  }
  if (options.tag === undefined) throw new Error("--tag is required");
  if (!Number.isSafeInteger(options.waitCiSeconds) || options.waitCiSeconds < 0 || options.waitCiSeconds > 3600) {
    throw new Error("--wait-ci-seconds must be an integer from 0 through 3600");
  }
  return options;
}

export function runReleaseDoctor(options) {
  const root = resolve(options.root);
  const source = inspectReleaseSource({
    root,
    tag: options.tag,
    updaterPublicKey: options.updaterPublicKey,
  });
  if (options.requirePublicChannel) {
    inspectPublicChannel(root, source.tag, source.version);
  }
  const head = run("git", ["rev-parse", "HEAD"], root);

  if (options.requireClean) {
    const status = run("git", ["status", "--porcelain", "--untracked-files=all"], root);
    if (status !== "") throw new Error("the release worktree must be clean");
  }

  let localTagCommit;
  if (options.requireHeadTag || options.requireRemoteTag) {
    localTagCommit = run("git", ["rev-parse", `${options.tag}^{commit}`], root);
    if (options.requireHeadTag && localTagCommit !== head) {
      throw new Error(`HEAD ${head} does not match ${options.tag} commit ${localTagCommit}`);
    }
  }

  const needsRemote = options.requireOriginMain || options.requireRemoteTag || options.requireCi;
  const repository = needsRemote
    ? options.repository ?? process.env.GITHUB_REPOSITORY ?? inferRepository(root)
    : options.repository;
  if (repository !== undefined && !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)) {
    throw new Error(`invalid GitHub repository: ${repository}`);
  }

  if (options.requireOriginMain) {
    const branch = run("git", ["branch", "--show-current"], root);
    if (branch !== "main") throw new Error(`release preparation must run on main, found ${branch || "detached HEAD"}`);
    const remoteMain = run(
      "gh",
      ["api", `repos/${repository}/commits/main`, "--jq", ".sha"],
      root,
    );
    if (remoteMain !== head) throw new Error(`HEAD ${head} does not match remote main ${remoteMain}`);
  }

  if (options.requireRemoteTag) {
    const remoteTagCommit = run(
      "gh",
      ["api", `repos/${repository}/commits/${options.tag}`, "--jq", ".sha"],
      root,
    );
    if (remoteTagCommit !== localTagCommit) {
      throw new Error(`remote ${options.tag} commit ${remoteTagCommit} does not match ${localTagCommit}`);
    }
  }

  if (options.requireCi) {
    const workflows = options.workflows.length === 0
      ? [DEFAULT_REQUIRED_WORKFLOW]
      : [...new Set(options.workflows)];
    requireExactShaWorkflows({ root, repository, commit: head, workflows }, options.waitCiSeconds);
  } else if (options.workflows.length > 0) {
    throw new Error("--require-workflow requires --require-ci");
  } else if (options.waitCiSeconds > 0) {
    throw new Error("--wait-ci-seconds requires --require-ci");
  }

  return { ...source, commit: head, repository: repository ?? null };
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const options = parseArgs(process.argv.slice(2));
    if (options.help) {
      console.log(usage());
    } else {
      const result = runReleaseDoctor(options);
      console.log(`release doctor passed: ${result.tag} @ ${result.commit}`);
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    console.error(usage());
    process.exitCode = 1;
  }
}
