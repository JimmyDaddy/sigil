#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { selectPublishedDesktopBeta } from "./select-published-desktop-beta.mjs";
import {
  assertReleaseChannelMonotonic,
  compareReleaseVersions,
} from "./release-channel-version.mjs";

const root = resolve(import.meta.dirname, "..");
const bundleConfig = JSON.parse(
  readFileSync(resolve(root, "apps/desktop/src-tauri/tauri.bundle.conf.json"), "utf8"),
);
const desktopWorkflow = readFileSync(
  resolve(root, ".github/workflows/desktop-package.yml"),
  "utf8",
);
const packageScript = readFileSync(
  resolve(root, "scripts/package-desktop-macos-local.sh"),
  "utf8",
);
const workspaceManifest = readFileSync(resolve(root, "Cargo.toml"), "utf8");
const notarizeScript = readFileSync(
  resolve(root, "scripts/notarize-desktop-macos.sh"),
  "utf8",
);
const verifyScript = readFileSync(
  resolve(root, "scripts/verify-desktop-macos.sh"),
  "utf8",
);
const notarizeAppScript = readFileSync(
  resolve(root, "scripts/notarize-desktop-macos-app.sh"),
  "utf8",
);
const verifyAppScript = readFileSync(
  resolve(root, "scripts/verify-desktop-macos-app.sh"),
  "utf8",
);
const updaterManifestScript = readFileSync(
  resolve(root, "scripts/generate-desktop-update-manifest.mjs"),
  "utf8",
);
const updaterSignatureVerifier = readFileSync(
  resolve(root, "crates/sigil-updater/src/bin/sigil-verify-tauri-update.rs"),
  "utf8",
);
const updaterSignatureScript = readFileSync(
  resolve(root, "scripts/verify-desktop-update-signature.sh"),
  "utf8",
);
const pagesWorkflow = readFileSync(
  resolve(root, ".github/workflows/pages.yml"),
  "utf8",
);
const releaseWorkflow = readFileSync(
  resolve(root, ".github/workflows/release.yml"),
  "utf8",
);
const publishedSmokeWorkflow = readFileSync(
  resolve(root, ".github/workflows/published-distribution-smoke.yml"),
  "utf8",
);
const npmPublishScript = readFileSync(
  resolve(root, "scripts/publish-npm-packages.sh"),
  "utf8",
);

assert.equal(bundleConfig.bundle.macOS.hardenedRuntime, true);
assert.equal(
  Object.hasOwn(bundleConfig.bundle.macOS, "signingIdentity"),
  false,
  "the shared bundle config must not force ad-hoc signing over APPLE_SIGNING_IDENTITY",
);

assert.match(desktopWorkflow, /APPLE_SIGNING_IDENTITY:[^\n]*'-'/);
assert.match(desktopWorkflow, /Signature=adhoc/);

const desktopVersion = JSON.parse(
  readFileSync(resolve(root, "apps/desktop/package.json"), "utf8"),
).version;
const workspaceVersion = workspaceManifest.match(
  /^\[workspace\.package\][\s\S]*?^version = "([^"]+)"/m,
)?.[1];
assert.equal(
  desktopVersion,
  workspaceVersion,
  "desktop and Cargo workspace versions must match before signing",
);

for (const token of [
  "Developer ID Application:",
  "APPLE_SIGNING_IDENTITY",
  "security find-identity",
  "--target all",
  "verify-desktop-macos.sh",
  "notarize-desktop-macos.sh",
  "notarize-desktop-macos-app.sh",
  'TAURI_SIGNING_PRIVATE_KEY="$(<"$updater_key")"',
  'TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$updater_key_password"',
  "TAURI_SIGNING_PRIVATE_KEY_PATH",
  "tauri.updater.conf.json",
  ".app.tar.gz",
  "verify-desktop-update-signature.sh",
  "swapped Desktop updater signature",
  "--expected-version",
  "--expected-commit",
]) {
  assert.match(packageScript, new RegExp(token.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&")));
}

for (const token of [
  "notarytool submit",
  "notarytool wait",
  "stapler staple",
  "verify-desktop-macos-app.sh",
]) {
  assert.match(notarizeAppScript, new RegExp(token.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&")));
}

for (const token of [
  "notarytool submit",
  "notarytool wait",
  "notarytool log",
  "stapler staple",
  "--submission-id",
  "verify-desktop-macos.sh",
]) {
  assert.match(notarizeScript, new RegExp(token.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&")));
}

for (const token of [
  "Authority=Developer ID Application:",
  "TeamIdentifier=",
  "Timestamp=",
  "Runtime Version=",
  "com.apple.security.get-task-allow",
  "stapler validate",
  "spctl --assess --type open",
  "spctl --assess --type execute",
  "--expected-version",
  "--expected-commit",
  "must be supplied together",
  "Sigil runtime version mismatch",
  "Sigil runtime commit mismatch",
]) {
  assert.match(verifyScript, new RegExp(token.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&")));
}

for (const token of [
  "Authority=Developer ID Application:",
  "TeamIdentifier=",
  "Runtime Version=",
  "stapler validate",
  "spctl --assess --type execute",
  "--expected-version",
  "--expected-commit",
  "must be supplied together",
  "Sigil runtime version mismatch",
  "Sigil runtime commit mismatch",
]) {
  assert.match(verifyAppScript, new RegExp(token.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&")));
}

for (const token of [
  "darwin-aarch64",
  "darwin-x86_64",
  ".app.tar.gz",
  ".sig",
  "pub_date",
  "verifySignature",
  "swapped Desktop updater signature",
]) {
  assert.match(
    updaterManifestScript,
    new RegExp(token.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&")),
  );
}

for (const token of [
  'pointer("/plugins/updater/pubkey")',
  "PublicKey::decode",
  "Signature::decode",
  "public_key.verify(&archive, &signature, true)",
  "ExitCode::from(1)",
]) {
  assert.match(
    updaterSignatureVerifier,
    new RegExp(token.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&")),
  );
}
for (const token of [
  "--locked",
  "-p sigil-updater",
  "--features release-verifier",
  "--bin sigil-verify-tauri-update",
  "apps/desktop/src-tauri/tauri.conf.json",
]) {
  assert.match(
    updaterSignatureScript,
    new RegExp(token.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&")),
  );
}

for (const token of [
  "--paginate",
  "--slurp",
  "releases?per_page=100",
  "select-published-desktop-beta.mjs",
  "entry.url !== expectedUrl",
]) {
  assert.match(pagesWorkflow, new RegExp(token.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&")));
}
for (const token of [
  "group: release-${{ github.event_name == 'workflow_dispatch' && inputs.tag || github.ref_name }}",
  "verify-desktop-macos:",
  "runs-on: ${{ matrix.os }}",
  "scripts/verify-desktop-macos.sh",
  "scripts/verify-desktop-macos-app.sh",
  "extract-verified-desktop-updater-app.py",
  "Record verified Desktop candidate digests",
  "Recheck verified Desktop candidate bytes",
  "--notarized",
  "APPLE_TEAM_ID: 9Q6HK7T78S",
  "publish-release:",
  "needs.verify-desktop-macos.result == 'success'",
  "release-channel-version.mjs",
  "Refuse Desktop update rollback",
  "--channel any",
  "Verify immutable releases policy",
  "repos/${GITHUB_REPOSITORY}/immutable-releases",
  "Verify published release immutability",
  "releases/tags/${RELEASE_TAG}",
  ".immutable // false",
  "X-GitHub-Api-Version: 2026-03-10",
  "SIGIL_RELEASE_ADMIN_TOKEN",
  "generate-desktop-update-manifest.mjs",
]) {
  assert.match(releaseWorkflow, new RegExp(token.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&")));
}
for (const token of [
  "Checkout published tag",
  "RELEASE_CHANNEL: beta",
  ".immutable // false",
  "verify-desktop-update-signature.sh",
  "swapped Desktop updater signature",
  "entry.signature !== signature",
]) {
  assert.match(
    publishedSmokeWorkflow,
    new RegExp(token.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&")),
  );
}
for (const token of [
  "cannot prove npm dist-tag state",
  "desired dist-tag state verified",
  "release-channel-version.mjs",
  "did not converge",
]) {
  assert.match(
    npmPublishScript,
    new RegExp(token.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&")),
  );
}

assert.equal(compareReleaseVersions("0.10.0-beta.10", "0.10.0-beta.2"), 1);
assert.equal(compareReleaseVersions("0.10.0-beta.0", "0.9.99-beta.99"), 1);
assert.equal(compareReleaseVersions("1.0.0-beta.0", "1.0.0-alpha.99"), 1);
assert.equal(compareReleaseVersions("1.0.0", "1.0.0-beta.99"), 1);
assert.equal(
  assertReleaseChannelMonotonic({
    candidate: "0.10.0-beta.10",
    channel: "beta",
    observed: ["0.9.0-beta.99", "0.10.0-beta.2"],
    surface: "test",
  }),
  "0.10.0-beta.2",
);
assert.throws(
  () =>
    assertReleaseChannelMonotonic({
      candidate: "0.10.0-beta.9",
      channel: "beta",
      observed: ["0.10.0-beta.10"],
      surface: "test",
    }),
  /refuses rollback/,
);
assert.throws(
  () =>
    assertReleaseChannelMonotonic({
      candidate: "0.10.0-alpha.10",
      channel: "beta",
      observed: [],
      surface: "test",
    }),
  /does not belong/,
);

const repository = "JimmyDaddy/sigil";
const betaRelease = (version, options = {}) => {
  const tag = `v${version}`;
  const assetNames = [
    "latest.json",
    `Sigil_${version}_aarch64-apple-darwin.app.tar.gz`,
    `Sigil_${version}_aarch64-apple-darwin.app.tar.gz.sig`,
    `Sigil_${version}_x86_64-apple-darwin.app.tar.gz`,
    `Sigil_${version}_x86_64-apple-darwin.app.tar.gz.sig`,
  ];
  return {
    draft: options.draft ?? false,
    prerelease: options.prerelease ?? true,
    immutable: options.immutable ?? true,
    tag_name: tag,
    assets: assetNames
      .filter((name) => name !== options.omitAsset)
      .map((name) => ({
        name,
        state: "uploaded",
        size: 128,
        browser_download_url:
          options.unsafeAsset === name
            ? `https://example.com/${name}`
            : `https://github.com/${repository}/releases/download/${tag}/${name}`,
      })),
  };
};

assert.deepEqual(
  selectPublishedDesktopBeta(
    [
      [betaRelease("0.9.0-beta.99"), betaRelease("9.0.0-beta.0", { draft: true })],
      [betaRelease("0.10.0-beta.2"), betaRelease("0.10.0-beta.10")],
    ],
    repository,
  ),
  {
    tag: "v0.10.0-beta.10",
    version: "0.10.0-beta.10",
  },
  "published beta selection must use SemVer maximum across every API page",
);
assert.throws(
  () =>
    selectPublishedDesktopBeta(
      [
        [betaRelease("0.10.0-beta.9")],
        [
          betaRelease("0.10.0-beta.10", {
            unsafeAsset: "latest.json",
          }),
        ],
      ],
      repository,
    ),
  /not safe and complete/,
  "an unsafe maximum beta must fail closed instead of falling back",
);
assert.throws(
  () =>
    selectPublishedDesktopBeta(
      [[betaRelease("0.10.0-beta.10", { immutable: false })]],
      repository,
    ),
  /not immutable/,
);
assert.throws(
  () =>
    selectPublishedDesktopBeta(
      [
        [betaRelease("0.10.0-beta.9")],
        [betaRelease("0.10.0-beta.10", { prerelease: false })],
      ],
      repository,
    ),
  /not marked as a prerelease/,
  "an incorrectly classified maximum beta must fail instead of rolling back",
);
assert.equal(selectPublishedDesktopBeta([[]], repository), null);

console.log("desktop macOS signing contract: ok");
