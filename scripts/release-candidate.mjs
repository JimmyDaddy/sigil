#!/usr/bin/env node

import { createHash } from "node:crypto";
import {
  lstatSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { basename, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { parseReleaseVersion } from "./release-channel-version.mjs";

const TARGETS = [
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "x86_64-pc-windows-msvc",
  "x86_64-unknown-linux-gnu",
];
const NPM_PLATFORMS = ["darwin-arm64", "darwin-x64", "linux-x64", "win32-x64"];

function usage() {
  return `Usage:
  scripts/release-candidate.mjs create --tag TAG --commit SHA --asset-dir DIR --output FILE
  scripts/release-candidate.mjs verify --manifest FILE --tag TAG --commit SHA [--asset-dir DIR] [--release-json FILE] [--require-draft]`;
}

function hashFile(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function assertRegularFile(path) {
  const metadata = lstatSync(path);
  if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size === 0) {
    throw new Error(`expected a non-empty regular file: ${path}`);
  }
  return metadata;
}

function expectedAssetNames(version) {
  return [
    ...TARGETS.flatMap((target) => [
      `sigil-${version}-${target}.tar.gz`,
      `sigil-${version}-${target}.tar.gz.sha256`,
    ]),
    "checksums.txt",
    "sigil-ai.rb",
    `sigil-ai-sigil-${version}.tgz`,
    ...NPM_PLATFORMS.map((platform) => `sigil-ai-sigil-${platform}-${version}.tgz`),
  ].sort();
}

function parseChecksumLine(contents, source) {
  const match = /^([0-9a-fA-F]{64})\s+[ *]?(.+?)\s*$/.exec(contents.trim());
  if (match === null) throw new Error(`invalid checksum line in ${source}`);
  return { sha256: match[1].toLowerCase(), name: basename(match[2]) };
}

function validateChecksums(assetDirectory, version) {
  const archiveNames = TARGETS.map((target) => `sigil-${version}-${target}.tar.gz`).sort();
  const observed = [];
  for (const archiveName of archiveNames) {
    const archive = resolve(assetDirectory, archiveName);
    const sidecar = `${archive}.sha256`;
    assertRegularFile(archive);
    assertRegularFile(sidecar);
    const checksum = parseChecksumLine(readFileSync(sidecar, "utf8"), sidecar);
    if (checksum.name !== archiveName || checksum.sha256 !== hashFile(archive)) {
      throw new Error(`checksum sidecar does not match ${archiveName}`);
    }
    observed.push(checksum);
  }

  const aggregatePath = resolve(assetDirectory, "checksums.txt");
  assertRegularFile(aggregatePath);
  const aggregate = readFileSync(aggregatePath, "utf8")
    .split(/\r?\n/)
    .filter((line) => line.trim() !== "")
    .map((line) => parseChecksumLine(line, aggregatePath))
    .sort((left, right) => left.name.localeCompare(right.name));
  if (JSON.stringify(aggregate) !== JSON.stringify(observed)) {
    throw new Error("checksums.txt does not exactly match the four release archives");
  }
}

function parseIdentity(tag, commit) {
  if (!/^v[^\s]+$/.test(tag)) throw new Error(`release tag must be v-prefixed: ${tag}`);
  const version = tag.slice(1);
  parseReleaseVersion(version);
  if (!/^[0-9a-f]{40}$/.test(commit)) throw new Error(`release commit must be a full lowercase SHA: ${commit}`);
  return { tag, version, commit };
}

export function createReleaseCandidateManifest({ tag, commit, assetDirectory, output }) {
  const identity = parseIdentity(tag, commit);
  const directory = resolve(assetDirectory);
  const outputPath = resolve(output);
  const expectedNames = expectedAssetNames(identity.version);
  const observedNames = readdirSync(directory)
    .filter((name) => resolve(directory, name) !== outputPath)
    .sort();
  if (JSON.stringify(observedNames) !== JSON.stringify(expectedNames)) {
    throw new Error(
      `release candidate asset inventory mismatch\nexpected: ${expectedNames.join(", ")}\nobserved: ${observedNames.join(", ")}`,
    );
  }
  validateChecksums(directory, identity.version);
  const assets = expectedNames.map((name) => {
    const path = resolve(directory, name);
    const metadata = assertRegularFile(path);
    return { name, sha256: hashFile(path), size: metadata.size };
  });
  const manifest = {
    schema_version: 1,
    ...identity,
    assets,
  };
  writeFileSync(outputPath, `${JSON.stringify(manifest, null, 2)}\n`, {
    encoding: "utf8",
    mode: 0o644,
  });
  return manifest;
}

function readAndValidateManifest(path, tag, commit) {
  assertRegularFile(path);
  const manifest = JSON.parse(readFileSync(path, "utf8"));
  const identity = parseIdentity(tag, commit);
  if (
    manifest.schema_version !== 1
    || manifest.tag !== identity.tag
    || manifest.version !== identity.version
    || manifest.commit !== identity.commit
  ) {
    throw new Error("release candidate identity does not match the requested tag and commit");
  }
  const expectedNames = expectedAssetNames(identity.version);
  if (!Array.isArray(manifest.assets)) throw new Error("release candidate assets must be an array");
  const names = manifest.assets.map((asset) => asset?.name);
  if (JSON.stringify(names) !== JSON.stringify(expectedNames)) {
    throw new Error("release candidate manifest has an unexpected asset inventory");
  }
  for (const asset of manifest.assets) {
    if (
      typeof asset.name !== "string"
      || !/^[0-9a-f]{64}$/.test(asset.sha256)
      || !Number.isSafeInteger(asset.size)
      || asset.size <= 0
    ) {
      throw new Error(`invalid release candidate asset descriptor: ${asset?.name ?? "<unknown>"}`);
    }
  }
  return manifest;
}

function expectedDesktopAssetNames(version) {
  return ["aarch64-apple-darwin", "x86_64-apple-darwin"].flatMap((target) => [
    `Sigil_${version}_${target}.dmg`,
    `Sigil_${version}_${target}.dmg.sha256`,
    `Sigil_${version}_${target}.app.tar.gz`,
    `Sigil_${version}_${target}.app.tar.gz.sha256`,
    `Sigil_${version}_${target}.app.tar.gz.sig`,
  ]);
}

export function verifyReleaseCandidateManifest({
  manifestPath,
  tag,
  commit,
  assetDirectory,
  releaseJsonPath,
  requireDraft = false,
}) {
  const manifest = readAndValidateManifest(resolve(manifestPath), tag, commit);
  if (assetDirectory !== undefined) {
    const directory = resolve(assetDirectory);
    for (const asset of manifest.assets) {
      const path = resolve(directory, asset.name);
      const metadata = assertRegularFile(path);
      if (metadata.size !== asset.size || hashFile(path) !== asset.sha256) {
        throw new Error(`release candidate bytes do not match the manifest: ${asset.name}`);
      }
    }
    validateChecksums(directory, manifest.version);
  }

  if (releaseJsonPath !== undefined) {
    const release = JSON.parse(readFileSync(resolve(releaseJsonPath), "utf8"));
    if (release.tagName !== tag) throw new Error(`GitHub Release tag does not match ${tag}`);
    if (requireDraft && release.isDraft !== true) throw new Error(`${tag} is not a draft release`);
    if (release.isDraft !== true && release.isImmutable !== true) {
      throw new Error(`${tag} is public but not immutable`);
    }
    if (!Array.isArray(release.assets)) throw new Error("GitHub Release assets must be an array");
    const remoteAssets = new Map(release.assets.map((asset) => [asset.name, asset]));
    for (const asset of manifest.assets) {
      const remote = remoteAssets.get(asset.name);
      if (
        remote?.state !== "uploaded"
        || remote.size !== asset.size
        || remote.digest !== `sha256:${asset.sha256}`
      ) {
        throw new Error(`GitHub Release asset does not match candidate manifest: ${asset.name}`);
      }
    }
    const allowed = new Set([
      ...manifest.assets.map((asset) => asset.name),
      `sigil-${manifest.version}-candidate.json`,
    ]);
    if (manifest.version.includes("-beta.")) {
      for (const name of expectedDesktopAssetNames(manifest.version)) allowed.add(name);
      allowed.add("latest.json");
    }
    const unexpected = [...remoteAssets.keys()].filter((name) => !allowed.has(name)).sort();
    if (unexpected.length > 0) {
      throw new Error(`GitHub Release contains unexpected assets: ${unexpected.join(", ")}`);
    }
  }
  return manifest;
}

function parseCliArgs(args) {
  const command = args[0];
  if (command === "-h" || command === "--help") return { help: true };
  if (command !== "create" && command !== "verify") throw new Error("expected create or verify");
  const values = new Map();
  let requireDraft = false;
  for (let index = 1; index < args.length; index += 1) {
    const flag = args[index];
    if (flag === "--require-draft") {
      requireDraft = true;
      continue;
    }
    const value = args[index + 1];
    if (!flag?.startsWith("--") || value === undefined || values.has(flag)) {
      throw new Error(`invalid argument: ${flag ?? "<missing>"}`);
    }
    values.set(flag, value);
    index += 1;
  }
  return { command, values, requireDraft };
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const parsed = parseCliArgs(process.argv.slice(2));
    if (parsed.help) {
      console.log(usage());
    } else if (parsed.command === "create") {
      const options = {
        tag: parsed.values.get("--tag"),
        commit: parsed.values.get("--commit"),
        assetDirectory: parsed.values.get("--asset-dir"),
        output: parsed.values.get("--output"),
      };
      if (Object.values(options).some((value) => value === undefined) || parsed.values.size !== 4) {
        throw new Error("create requires --tag, --commit, --asset-dir, and --output");
      }
      createReleaseCandidateManifest(options);
      console.log(`release candidate manifest written to ${resolve(options.output)}`);
    } else {
      const options = {
        manifestPath: parsed.values.get("--manifest"),
        tag: parsed.values.get("--tag"),
        commit: parsed.values.get("--commit"),
        assetDirectory: parsed.values.get("--asset-dir"),
        releaseJsonPath: parsed.values.get("--release-json"),
        requireDraft: parsed.requireDraft,
      };
      const allowed = new Set(["--manifest", "--tag", "--commit", "--asset-dir", "--release-json"]);
      if (
        options.manifestPath === undefined
        || options.tag === undefined
        || options.commit === undefined
        || [...parsed.values.keys()].some((flag) => !allowed.has(flag))
      ) {
        throw new Error("verify requires --manifest, --tag, and --commit");
      }
      verifyReleaseCandidateManifest(options);
      console.log(`release candidate verified: ${options.tag} @ ${options.commit}`);
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    console.error(usage());
    process.exitCode = 1;
  }
}
