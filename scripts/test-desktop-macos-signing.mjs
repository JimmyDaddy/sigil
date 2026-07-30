#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

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
]) {
  assert.match(packageScript, new RegExp(token.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&")));
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
]) {
  assert.match(verifyScript, new RegExp(token.replaceAll(/[.*+?^${}()|[\]\\]/g, "\\$&")));
}

console.log("desktop macOS signing contract: ok");
