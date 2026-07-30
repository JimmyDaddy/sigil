#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const VERSION_PATTERN =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-(alpha|beta)\.(0|[1-9]\d*))?$/;

export function parseReleaseVersion(version) {
  const match = VERSION_PATTERN.exec(version);
  if (match === null) {
    throw new Error(`unsupported release version: ${version}`);
  }
  return {
    raw: version,
    major: BigInt(match[1]),
    minor: BigInt(match[2]),
    patch: BigInt(match[3]),
    channel: match[4] ?? "stable",
    prerelease: match[5] === undefined ? null : BigInt(match[5]),
  };
}

export function compareReleaseVersions(leftVersion, rightVersion) {
  const left = typeof leftVersion === "string"
    ? parseReleaseVersion(leftVersion)
    : leftVersion;
  const right = typeof rightVersion === "string"
    ? parseReleaseVersion(rightVersion)
    : rightVersion;
  for (const field of ["major", "minor", "patch"]) {
    if (left[field] < right[field]) return -1;
    if (left[field] > right[field]) return 1;
  }
  const channelRank = new Map([
    ["alpha", 0],
    ["beta", 1],
    ["stable", 2],
  ]);
  const leftRank = channelRank.get(left.channel);
  const rightRank = channelRank.get(right.channel);
  if (leftRank < rightRank) return -1;
  if (leftRank > rightRank) return 1;
  if (left.prerelease === null) return 0;
  if (left.prerelease < right.prerelease) return -1;
  if (left.prerelease > right.prerelease) return 1;
  return 0;
}

export function assertReleaseChannelMonotonic({
  candidate,
  channel,
  observed = [],
  surface = "release channel",
}) {
  if (!["alpha", "beta", "stable", "any"].includes(channel)) {
    throw new Error(`unsupported release channel: ${channel}`);
  }
  const parsedCandidate = parseReleaseVersion(candidate);
  if (channel !== "any" && parsedCandidate.channel !== channel) {
    throw new Error(
      `${surface} candidate ${candidate} does not belong to ${channel}`,
    );
  }
  if (!Array.isArray(observed) || !observed.every((value) => typeof value === "string")) {
    throw new Error(`${surface} observed versions must be an array of strings`);
  }

  const relevant = observed
    .map((version) => parseReleaseVersion(version))
    .filter((version) => channel === "any" || version.channel === channel)
    .sort(compareReleaseVersions);
  const current = relevant.at(-1);
  if (current !== undefined && compareReleaseVersions(parsedCandidate, current) < 0) {
    throw new Error(
      `${surface} refuses rollback from ${current.raw} to ${candidate}`,
    );
  }
  return current?.raw ?? null;
}

function parseCliArgs(args) {
  const values = new Map();
  for (let index = 0; index < args.length; index += 2) {
    const flag = args[index];
    const value = args[index + 1];
    if (!flag?.startsWith("--") || value === undefined || values.has(flag)) {
      throw new Error(
        "usage: release-channel-version.mjs --candidate VERSION --channel alpha|beta|stable|any [--current VERSION | --observed-file PATH] [--surface LABEL]",
      );
    }
    values.set(flag, value);
  }
  const candidate = values.get("--candidate");
  const channel = values.get("--channel");
  const current = values.get("--current");
  const observedFile = values.get("--observed-file");
  const surface = values.get("--surface") ?? "release channel";
  const allowed = new Set([
    "--candidate",
    "--channel",
    "--current",
    "--observed-file",
    "--surface",
  ]);
  if (
    candidate === undefined
    || channel === undefined
    || [...values.keys()].some((flag) => !allowed.has(flag))
    || (current !== undefined && observedFile !== undefined)
  ) {
    throw new Error(
      "usage: release-channel-version.mjs --candidate VERSION --channel alpha|beta|stable|any [--current VERSION | --observed-file PATH] [--surface LABEL]",
    );
  }
  let observed = current === undefined ? [] : [current];
  if (observedFile !== undefined) {
    observed = JSON.parse(readFileSync(observedFile, "utf8"));
  }
  return { candidate, channel, observed, surface };
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    assertReleaseChannelMonotonic(parseCliArgs(process.argv.slice(2)));
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}
