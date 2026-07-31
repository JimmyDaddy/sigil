#!/usr/bin/env node

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";

const root = process.cwd();
const releaseScript = fs.readFileSync(
  path.join(root, "scripts/build-release-archive.sh"),
  "utf8",
);
const npmLauncher = fs.readFileSync(
  path.join(root, "npm/sigil/bin/sigil.js"),
  "utf8",
);
const ciWorkflow = fs.readFileSync(
  path.join(root, ".github/workflows/ci.yml"),
  "utf8",
);
const desktopPackageWorkflow = fs.readFileSync(
  path.join(root, ".github/workflows/desktop-package.yml"),
  "utf8",
);

assert.match(
  releaseScript,
  /SIGIL_BUILD_DISTRIBUTION="github-release"\s*\\/,
  "release archives must embed the standalone distribution marker",
);
assert.match(
  npmLauncher,
  /SIGIL_INSTALL_SOURCE:\s*"npm"/,
  "the npm launcher must preserve npm ownership for update policy",
);

for (const pathFilter of [
  ".github/workflows/desktop-package.yml",
  ".github/workflows/pages.yml",
  ".github/workflows/published-distribution-smoke.yml",
  ".github/workflows/release.yml",
  "scripts/build-release-archive.sh",
  "scripts/extract-verified-desktop-updater-app.py",
  "scripts/generate-desktop-update-manifest.mjs",
  "scripts/notarize-desktop-macos-app.sh",
  "scripts/notarize-desktop-macos.sh",
  "scripts/package-desktop-macos-local.sh",
  "scripts/upload-desktop-macos-release.sh",
  "scripts/release-candidate.mjs",
  "scripts/release-doctor.mjs",
  "scripts/release-channel-version.mjs",
  "scripts/publish-npm-packages.sh",
  "scripts/test-desktop-macos-signing.mjs",
  "scripts/test-extract-verified-desktop-updater-app.py",
  "scripts/test-published-distribution-smoke.sh",
  "scripts/test-publish-npm-packages.sh",
  "scripts/test-release-tooling.mjs",
  "scripts/test-update-distribution-markers.mjs",
  "scripts/verify-desktop-macos-app.sh",
  "scripts/verify-desktop-macos.sh",
]) {
  assert.match(
    ciWorkflow,
    new RegExp(`- "${pathFilter.replaceAll(".", "\\.")}"`, "g"),
    `${pathFilter} must trigger the read-only CI contract audit`,
  );
}

for (const command of [
  "python3 scripts/test-extract-verified-desktop-updater-app.py",
  "node scripts/test-update-distribution-markers.mjs",
  "scripts/test-published-distribution-smoke.sh",
  "node scripts/test-release-tooling.mjs",
  "scripts/test-publish-npm-packages.sh",
]) {
  assert.match(
    ciWorkflow,
    new RegExp(command.replaceAll(".", "\\.")),
    `${command} must run in the CI contract job`,
  );
}

for (const pathFilter of [
  "scripts/extract-verified-desktop-updater-app.py",
  "scripts/notarize-desktop-macos-app.sh",
  "scripts/notarize-desktop-macos.sh",
  "scripts/package-desktop-macos-local.sh",
  "scripts/release-doctor.mjs",
  "scripts/release-channel-version.mjs",
  "scripts/test-desktop-macos-signing.mjs",
  "scripts/test-extract-verified-desktop-updater-app.py",
  "scripts/verify-desktop-macos-app.sh",
  "scripts/verify-desktop-macos.sh",
]) {
  assert.match(
    desktopPackageWorkflow,
    new RegExp(`- "${pathFilter.replaceAll(".", "\\.")}"`, "g"),
    `${pathFilter} must trigger Desktop package validation`,
  );
}

console.log("update distribution marker checks passed");
