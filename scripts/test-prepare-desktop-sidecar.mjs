#!/usr/bin/env node

import assert from "node:assert/strict";
import path from "node:path";

import {
  desktopSidecarPaths,
  parseDesktopSidecarArgs,
} from "./prepare-desktop-sidecar.mjs";

assert.deepEqual(parseDesktopSidecarArgs([]), {
  profile: "release",
  target: undefined,
  skipBuild: false,
});
assert.deepEqual(
  parseDesktopSidecarArgs([
    "--target",
    "x86_64-pc-windows-msvc",
    "--profile",
    "dogfood",
    "--skip-build",
  ]),
  {
    profile: "dogfood",
    target: "x86_64-pc-windows-msvc",
    skipBuild: true,
  },
);
assert.throws(() => parseDesktopSidecarArgs(["--target", "../escape"]));
assert.throws(() => parseDesktopSidecarArgs(["--unknown"]));

const windows = desktopSidecarPaths("/repo", "x86_64-pc-windows-msvc", "release");
assert.equal(windows.source, path.join("/repo", "target", "x86_64-pc-windows-msvc", "release", "sigil.exe"));
assert.equal(
  windows.destination,
  path.join(
    "/repo",
    "apps",
    "desktop",
    "src-tauri",
    "binaries",
    "sigil-runtime-x86_64-pc-windows-msvc.exe",
  ),
);

const macOS = desktopSidecarPaths("/repo", "aarch64-apple-darwin", "release");
assert.equal(macOS.source, path.join("/repo", "target", "aarch64-apple-darwin", "release", "sigil"));
assert.equal(path.basename(macOS.destination), "sigil-runtime-aarch64-apple-darwin");

process.stdout.write("desktop sidecar preparation tests passed\n");
