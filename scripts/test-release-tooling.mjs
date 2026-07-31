#!/usr/bin/env node

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import {
  createReleaseCandidateManifest,
  verifyReleaseCandidateManifest,
} from "./release-candidate.mjs";
import { inspectReleaseSource } from "./release-doctor.mjs";

const temporaryRoot = mkdtempSync(join(tmpdir(), "sigil-release-tooling-"));
try {
  testReleaseDoctor(resolve(temporaryRoot, "doctor"));
  testReleaseCandidate(resolve(temporaryRoot, "candidate"));
  console.log("release tooling tests passed");
} finally {
  rmSync(temporaryRoot, { recursive: true, force: true });
}

function write(path, contents) {
  mkdirSync(resolve(path, ".."), { recursive: true });
  writeFileSync(path, contents);
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function testReleaseDoctor(root) {
  const version = "1.2.3-beta.4";
  mkdirSync(root, { recursive: true });
  write(resolve(root, "Cargo.toml"), `[workspace.package]\nversion = "${version}"\n`);
  write(
    resolve(root, "Cargo.lock"),
    ["sigil", "sigil-desktop-app", "sigil-runtime", "sigil-tui"]
      .map((name) => `[[package]]\nname = "${name}"\nversion = "${version}"\n`)
      .join("\n"),
  );
  write(resolve(root, "apps/desktop/package.json"), JSON.stringify({ version }));
  write(
    resolve(root, "apps/desktop/src-tauri/tauri.conf.json"),
    JSON.stringify({ version, plugins: { updater: { pubkey: "publisher-key" } } }),
  );
  write(resolve(root, "docs/en/changelog.md"), `## v${version} - 2026-08-01\n`);
  write(resolve(root, "docs/zh-CN/changelog.md"), `## v${version} - 2026-08-01\n`);
  const publicKey = resolve(root, "publisher.key.pub");
  write(publicKey, "publisher-key\n");

  assert.equal(
    inspectReleaseSource({ root, tag: `v${version}`, updaterPublicKey: publicKey }).version,
    version,
  );
  write(resolve(root, "apps/desktop/package.json"), JSON.stringify({ version: "1.2.3-beta.3" }));
  assert.throws(
    () => inspectReleaseSource({ root, tag: `v${version}`, updaterPublicKey: publicKey }),
    /Desktop package version/,
  );
}

function testReleaseCandidate(root) {
  const version = "1.2.3-beta.4";
  const tag = `v${version}`;
  const commit = "a".repeat(40);
  mkdirSync(root, { recursive: true });
  const targets = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
  ];
  const checksumLines = [];
  for (const target of targets) {
    const name = `sigil-${version}-${target}.tar.gz`;
    const path = resolve(root, name);
    write(path, `archive:${target}\n`);
    const line = `${sha256(path)}  dist/${name}`;
    write(`${path}.sha256`, `${line}\n`);
    checksumLines.push(line);
  }
  write(resolve(root, "checksums.txt"), `${checksumLines.sort().join("\n")}\n`);
  write(resolve(root, "sigil-ai.rb"), "class SigilAi < Formula; end\n");
  for (const name of [
    `sigil-ai-sigil-${version}.tgz`,
    `sigil-ai-sigil-darwin-arm64-${version}.tgz`,
    `sigil-ai-sigil-darwin-x64-${version}.tgz`,
    `sigil-ai-sigil-linux-x64-${version}.tgz`,
    `sigil-ai-sigil-win32-x64-${version}.tgz`,
  ]) {
    write(resolve(root, name), `npm:${name}\n`);
  }

  const manifestPath = resolve(root, `sigil-${version}-candidate.json`);
  const manifest = createReleaseCandidateManifest({
    tag,
    commit,
    assetDirectory: root,
    output: manifestPath,
  });
  assert.equal(manifest.assets.length, 15);
  verifyReleaseCandidateManifest({ manifestPath, tag, commit, assetDirectory: root });

  const releaseJsonPath = resolve(root, "release.json");
  const release = {
    tagName: tag,
    isDraft: true,
    isImmutable: false,
    assets: [
      ...manifest.assets.map((asset) => ({
        name: asset.name,
        size: asset.size,
        digest: `sha256:${asset.sha256}`,
        state: "uploaded",
      })),
      {
        name: `sigil-${version}-candidate.json`,
        size: readFileSync(manifestPath).length,
        digest: `sha256:${sha256(manifestPath)}`,
        state: "uploaded",
      },
    ],
  };
  write(releaseJsonPath, JSON.stringify(release));
  verifyReleaseCandidateManifest({
    manifestPath,
    tag,
    commit,
    releaseJsonPath,
    requireDraft: true,
  });

  release.assets[0].digest = `sha256:${"0".repeat(64)}`;
  write(releaseJsonPath, JSON.stringify(release));
  assert.throws(
    () => verifyReleaseCandidateManifest({ manifestPath, tag, commit, releaseJsonPath }),
    /does not match candidate manifest/,
  );
  assert.throws(
    () => verifyReleaseCandidateManifest({ manifestPath, tag, commit: "b".repeat(40) }),
    /identity does not match/,
  );
}
