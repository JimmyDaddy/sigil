#!/usr/bin/env node
"use strict";

const childProcess = require("node:child_process");
const path = require("node:path");

const packageByPlatform = {
  "darwin-arm64": "@jimmydaddy/sigil-darwin-arm64",
  "darwin-x64": "@jimmydaddy/sigil-darwin-x64",
  "linux-arm64": "@jimmydaddy/sigil-linux-arm64",
  "linux-x64": "@jimmydaddy/sigil-linux-x64",
  "win32-arm64": "@jimmydaddy/sigil-win32-arm64",
  "win32-x64": "@jimmydaddy/sigil-win32-x64",
};

const platformKey = `${process.platform}-${process.arch}`;
const packageName = packageByPlatform[platformKey];

if (!packageName) {
  console.error(`Sigil does not ship an npm binary for ${platformKey}.`);
  process.exit(1);
}

let packageJsonPath;
try {
  packageJsonPath = require.resolve(`${packageName}/package.json`);
} catch (error) {
  console.error(
    `Sigil binary package ${packageName} is not installed. Reinstall @jimmydaddy/sigil for ${platformKey}.`,
  );
  process.exit(1);
}

const binaryName = process.platform === "win32" ? "sigil.exe" : "sigil";
const binaryPath = path.join(path.dirname(packageJsonPath), "bin", binaryName);
const child = childProcess.spawn(binaryPath, process.argv.slice(2), {
  stdio: "inherit",
});

child.on("error", (error) => {
  console.error(`failed to start ${binaryPath}: ${error.message}`);
  process.exit(1);
});

child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }

  process.exit(code ?? 1);
});
