import type { MarkdownRepairDiagnostic } from "./types";

interface Fence {
  readonly marker: "`" | "~";
  readonly length: number;
  readonly indent: string;
}

export interface NormalizedMarkdown {
  readonly source: string;
  readonly diagnostics: readonly MarkdownRepairDiagnostic[];
}

export function normalizeCompletedMarkdown(source: string): NormalizedMarkdown {
  const lines = source.split("\n");
  const normalized: string[] = [];
  const diagnostics: MarkdownRepairDiagnostic[] = [];
  let fence: Fence | undefined;
  let sourceOffset = 0;

  for (const line of lines) {
    if (fence === undefined) {
      fence = openingFence(line);
      normalized.push(line);
      sourceOffset += utf8Length(line) + 1;
      continue;
    }

    if (isClosingFence(line, fence)) {
      fence = undefined;
      normalized.push(line);
      sourceOffset += utf8Length(line) + 1;
      continue;
    }

    const repaired = splitAttachedClosingFence(line, fence);
    if (repaired === undefined) {
      normalized.push(line);
      sourceOffset += utf8Length(line) + 1;
      continue;
    }

    const contentBytes = utf8Length(repaired.content);
    const markerBytes = utf8Length(repaired.closing.trimStart());
    const markerStart = sourceOffset + contentBytes;
    diagnostics.push({
      kind: "attached_closing_fence",
      sourceStart: markerStart,
      sourceEnd: markerStart + markerBytes,
    });
    normalized.push(repaired.content, repaired.closing);
    sourceOffset += utf8Length(line) + 1;
    fence = undefined;
  }

  return { source: normalized.join("\n"), diagnostics };
}

export function normalizeStreamingMarkdown(source: string): NormalizedMarkdown {
  return { source, diagnostics: [] };
}

export function openingFence(line: string): Fence | undefined {
  const match = line.match(/^( {0,3})(`{3,}|~{3,})(.*)$/u);
  if (match === null) return undefined;
  const run = match[2];
  const marker = run[0] as "`" | "~";
  if (marker === "`" && match[3].includes("`")) return undefined;
  return { marker, length: run.length, indent: match[1] };
}

export function isClosingFence(line: string, fence: Pick<Fence, "marker" | "length">): boolean {
  const trimmed = line.trim();
  return line.length - line.trimStart().length <= 3
    && trimmed.length >= fence.length
    && [...trimmed].every((character) => character === fence.marker);
}

function splitAttachedClosingFence(
  line: string,
  fence: Fence,
): { readonly content: string; readonly closing: string } | undefined {
  const withoutTrailingWhitespace = line.trimEnd();
  let markerStart = withoutTrailingWhitespace.length;
  while (markerStart > 0 && withoutTrailingWhitespace[markerStart - 1] === fence.marker) {
    markerStart -= 1;
  }
  if (
    withoutTrailingWhitespace.length - markerStart < fence.length
    || withoutTrailingWhitespace.slice(0, markerStart).trim() === ""
  ) return undefined;
  return {
    content: withoutTrailingWhitespace.slice(0, markerStart).trimEnd(),
    closing: `${fence.indent}${withoutTrailingWhitespace.slice(markerStart)}`,
  };
}

export function utf8Length(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}
