#!/usr/bin/env node

import { chmodSync, copyFileSync, mkdirSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(SCRIPT_DIR, "..");

export function parseDesktopSidecarArgs(argv) {
  const options = { profile: "release", target: undefined, skipBuild: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--skip-build") {
      options.skipBuild = true;
    } else if (argument === "--profile" || argument === "--target") {
      const value = argv[index + 1];
      if (!value || value.startsWith("--")) {
        throw new Error(`${argument} requires a value`);
      }
      options[argument === "--profile" ? "profile" : "target"] = value;
      index += 1;
    } else {
      throw new Error(`unknown argument: ${argument}`);
    }
  }
  if (!/^[a-z0-9][a-z0-9._-]{0,127}$/.test(options.profile)) {
    throw new Error("profile must be a bounded Cargo profile name");
  }
  if (options.target !== undefined && !/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(options.target)) {
    throw new Error("target must be a bounded Rust target triple");
  }
  return options;
}

export function desktopSidecarPaths(repoRoot, target, profile) {
  const extension = target.includes("windows") ? ".exe" : "";
  return {
    source: path.join(repoRoot, "target", target, profile, `sigil${extension}`),
    destination: path.join(
      repoRoot,
      "apps",
      "desktop",
      "src-tauri",
      "binaries",
      `sigil-runtime-${target}${extension}`,
    ),
  };
}

function run(command, args) {
  const result = spawnSync(command, args, { cwd: REPO_ROOT, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} exited with status ${result.status ?? "unknown"}`);
  }
}

function resolveHostTarget() {
  const result = spawnSync("rustc", ["--print", "host-tuple"], {
    cwd: REPO_ROOT,
    encoding: "utf8",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error("rustc could not report the host target tuple");
  const target = result.stdout.trim();
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(target)) {
    throw new Error("rustc returned an invalid host target tuple");
  }
  return target;
}

export function prepareDesktopSidecar(argv = process.argv.slice(2)) {
  const options = parseDesktopSidecarArgs(argv);
  const target = options.target ?? resolveHostTarget();
  const paths = desktopSidecarPaths(REPO_ROOT, target, options.profile);
  if (!options.skipBuild) {
    run("cargo", [
      "build",
      "--locked",
      "--package",
      "sigil",
      "--bin",
      "sigil",
      "--profile",
      options.profile,
      "--target",
      target,
    ]);
  }
  mkdirSync(path.dirname(paths.destination), { recursive: true });
  copyFileSync(paths.source, paths.destination);
  if (!target.includes("windows")) chmodSync(paths.destination, 0o755);
  process.stdout.write(`prepared desktop sidecar for ${target}\n`);
  return paths;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    prepareDesktopSidecar();
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  }
}
