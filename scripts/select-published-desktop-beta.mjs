#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const BETA_TAG_PATTERN =
  /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)-beta\.(0|[1-9]\d*)$/;

export function selectPublishedDesktopBeta(releasePages, repository) {
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)) {
    throw new Error(`invalid GitHub repository: ${repository}`);
  }
  if (!Array.isArray(releasePages) || !releasePages.every(Array.isArray)) {
    throw new Error("GitHub release pagination input must be an array of pages");
  }

  const candidates = releasePages
    .flat()
    .filter(
      (release) =>
        release !== null
        && typeof release === "object"
        && release.draft === false
        && typeof release.tag_name === "string"
        && BETA_TAG_PATTERN.test(release.tag_name),
    )
    .map((release) => ({
      release,
      semver: parseBetaSemver(release.tag_name),
    }));
  if (candidates.length === 0) {
    return null;
  }

  candidates.sort((left, right) => compareSemver(left.semver, right.semver));
  const selected = candidates.at(-1).release;
  if (candidates.filter(({ release }) => release.tag_name === selected.tag_name).length !== 1) {
    throw new Error(`duplicate published beta release: ${selected.tag_name}`);
  }
  validateDesktopAssets(selected, repository);
  return {
    tag: selected.tag_name,
    version: selected.tag_name.slice(1),
  };
}

function parseBetaSemver(tag) {
  const match = BETA_TAG_PATTERN.exec(tag);
  if (match === null) {
    throw new Error(`invalid Desktop beta tag: ${tag}`);
  }
  return match.slice(1).map((part) => BigInt(part));
}

function compareSemver(left, right) {
  for (let index = 0; index < left.length; index += 1) {
    if (left[index] < right[index]) return -1;
    if (left[index] > right[index]) return 1;
  }
  return 0;
}

function validateDesktopAssets(release, repository) {
  if (release.prerelease !== true) {
    throw new Error(`published beta is not marked as a prerelease: ${release.tag_name}`);
  }
  if (release.immutable !== true) {
    throw new Error(`published beta is not immutable: ${release.tag_name}`);
  }
  if (!Array.isArray(release.assets)) {
    throw new Error(`published beta has no asset inventory: ${release.tag_name}`);
  }

  const version = release.tag_name.slice(1);
  const expectedNames = [
    "latest.json",
    `Sigil_${version}_aarch64-apple-darwin.app.tar.gz`,
    `Sigil_${version}_aarch64-apple-darwin.app.tar.gz.sig`,
    `Sigil_${version}_x86_64-apple-darwin.app.tar.gz`,
    `Sigil_${version}_x86_64-apple-darwin.app.tar.gz.sig`,
  ];
  for (const name of expectedNames) {
    const matches = release.assets.filter((asset) => asset?.name === name);
    if (matches.length !== 1) {
      throw new Error(
        `published beta must contain exactly one ${name}; found ${matches.length}`,
      );
    }
    const asset = matches[0];
    const maximumSize = name === "latest.json"
      ? 65_536
      : name.endsWith(".sig")
        ? 4_096
        : 1_073_741_824;
    const expectedUrl =
      `https://github.com/${repository}/releases/download/${release.tag_name}/${name}`;
    if (
      asset.state !== "uploaded"
      || !Number.isSafeInteger(asset.size)
      || asset.size <= 0
      || asset.size > maximumSize
      || asset.browser_download_url !== expectedUrl
    ) {
      throw new Error(`published beta asset is not safe and complete: ${name}`);
    }
  }
}

function parseCliArgs(args) {
  const values = new Map();
  for (let index = 0; index < args.length; index += 2) {
    const flag = args[index];
    const value = args[index + 1];
    if (!flag?.startsWith("--") || value === undefined || values.has(flag)) {
      throw new Error(
        "usage: select-published-desktop-beta.mjs --repository OWNER/REPO --input PATH",
      );
    }
    values.set(flag, value);
  }
  const repository = values.get("--repository");
  const input = values.get("--input");
  if (repository === undefined || input === undefined || values.size !== 2) {
    throw new Error(
      "usage: select-published-desktop-beta.mjs --repository OWNER/REPO --input PATH",
    );
  }
  return { repository, input };
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const { repository, input } = parseCliArgs(process.argv.slice(2));
    const releasePages = JSON.parse(readFileSync(input, "utf8"));
    const selected = selectPublishedDesktopBeta(releasePages, repository);
    if (selected !== null) {
      process.stdout.write(`${selected.tag}\n`);
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}
