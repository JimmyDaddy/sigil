#!/usr/bin/env node

import {
  lstatSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, resolve } from "node:path";
import { spawnSync } from "node:child_process";

function usage() {
  console.error(`Usage: scripts/generate-desktop-update-manifest.mjs \\
  --version VERSION \\
  --artifact-dir PATH \\
  --output PATH \\
  [--repository OWNER/REPO] \\
  [--pub-date RFC3339] \\
  [--notes TEXT]`);
}

const values = new Map();
for (let index = 2; index < process.argv.length; index += 2) {
  const flag = process.argv[index];
  const value = process.argv[index + 1];
  if (!flag?.startsWith("--") || value === undefined) {
    usage();
    process.exit(2);
  }
  values.set(flag, value);
}

const version = values.get("--version");
const artifactDirectory = values.get("--artifact-dir");
const output = values.get("--output");
const repository = values.get("--repository") ?? "JimmyDaddy/sigil";
const pubDate = values.get("--pub-date") ?? new Date().toISOString();
const notes = values.get("--notes")
  ?? `Sigil Desktop ${version ?? ""} beta update. See the signed release notes for details.`;

if (
  version === undefined
  || artifactDirectory === undefined
  || output === undefined
  || !/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)-beta\.(0|[1-9]\d*)$/.test(version)
  || !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)
  || Number.isNaN(Date.parse(pubDate))
) {
  usage();
  process.exit(2);
}

const tag = `v${version}`;
const releaseRoot = `https://github.com/${repository}/releases/download/${tag}`;
const repositoryRoot = resolve(import.meta.dirname, "..");
const targets = [
  ["darwin-aarch64", "aarch64-apple-darwin"],
  ["darwin-x86_64", "x86_64-apple-darwin"],
];
const platforms = {};
const artifacts = [];

for (const [platform, target] of targets) {
  const archiveName = `Sigil_${version}_${target}.app.tar.gz`;
  const archivePath = resolve(artifactDirectory, archiveName);
  const signaturePath = `${archivePath}.sig`;
  assertRegularFile(archivePath);
  assertRegularFile(signaturePath);
  verifySignature(archivePath, signaturePath, false);
  const signature = readFileSync(signaturePath, "utf8").trim();
  if (
    !/^[A-Za-z0-9+/]+={0,2}$/.test(signature)
    || signature.length < 40
    || signature.length > 4096
  ) {
    throw new Error(`invalid updater signature: ${signaturePath}`);
  }
  artifacts.push({ archivePath, signaturePath });
  platforms[platform] = {
    signature,
    url: `${releaseRoot}/${encodeURIComponent(basename(archivePath))}`,
  };
}

verifySignature(artifacts[0].archivePath, artifacts[1].signaturePath, true);

const manifest = {
  version,
  notes,
  pub_date: new Date(pubDate).toISOString(),
  platforms,
};
mkdirSync(dirname(resolve(output)), { recursive: true });
writeFileSync(resolve(output), `${JSON.stringify(manifest, null, 2)}\n`, {
  encoding: "utf8",
  mode: 0o644,
});
console.log(`Desktop updater manifest written to ${resolve(output)}`);

function assertRegularFile(path) {
  const stat = lstatSync(path);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size === 0) {
    throw new Error(`expected a non-empty regular file: ${path}`);
  }
}

function verifySignature(archivePath, signaturePath, expectInvalid) {
  const result = spawnSync(
    "bash",
    [
      resolve(repositoryRoot, "scripts/verify-desktop-update-signature.sh"),
      "--archive",
      archivePath,
      "--signature",
      signaturePath,
    ],
    {
      cwd: repositoryRoot,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  if (result.error !== undefined) {
    throw result.error;
  }
  if (expectInvalid) {
    if (result.status !== 1) {
      throw new Error(
        result.status === 0
          ? "swapped Desktop updater signature unexpectedly verified"
          : `swapped-signature negative control did not reach cryptographic verification: ${result.stderr.trim()}`,
      );
    }
    return;
  }
  if (result.status !== 0) {
    throw new Error(
      `Desktop updater signature verification failed for ${archivePath}: ${result.stderr.trim()}`,
    );
  }
}
