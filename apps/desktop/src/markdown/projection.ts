import {
  isClosingFence,
  normalizeCompletedMarkdown,
  normalizeStreamingMarkdown,
  openingFence,
  utf8Length,
} from "./normalize";
import type {
  MarkdownPhase,
  MarkdownProjection,
  ProjectedMarkdownBlock,
  ProjectedMarkdownBlockKind,
} from "./types";

interface LineSlice {
  readonly text: string;
  readonly start: number;
  readonly end: number;
  readonly hasNewline: boolean;
}

interface BlockSlice {
  readonly start: number;
  readonly end: number;
  readonly kind: ProjectedMarkdownBlockKind;
  readonly closed: boolean;
}

export interface MarkdownProjectionCursor {
  readonly contentId: string;
  readonly phase: MarkdownPhase;
  readonly source: string;
  readonly projection: MarkdownProjection;
}

export interface MarkdownProjectionUpdate {
  readonly projection: MarkdownProjection;
  readonly cursor: MarkdownProjectionCursor;
  readonly reusedStableBlocks: number;
}

interface ProjectMarkdownInput {
  readonly source: string;
  readonly phase: MarkdownPhase;
  readonly contentId: string;
}

export function projectMarkdown(input: ProjectMarkdownInput): MarkdownProjection {
  return projectMarkdownWithCursor(input).projection;
}

export function projectMarkdownWithCursor({
  source,
  phase,
  contentId,
}: ProjectMarkdownInput, previous?: MarkdownProjectionCursor): MarkdownProjectionUpdate {
  const reusablePrefix = reusableStablePrefix(previous, source, phase, contentId);
  const normalized = phase === "complete"
    ? normalizeCompletedMarkdown(source)
    : normalizeStreamingMarkdown(source);
  const scanStart = reusablePrefix.length === 0
    ? 0
    : reusablePrefix[reusablePrefix.length - 1].sourceEnd;
  const blocks = scanBlocks(utf8Slice(normalized.source, scanStart, utf8Length(normalized.source)))
    .map((block) => ({
      ...block,
      start: block.start + scanStart,
      end: block.end + scanStart,
    }));
  const finalBlockIndex = reusablePrefix.length + blocks.length - 1;
  const projectedBlocks = [
    ...reusablePrefix,
    ...blocks.map((block, relativeIndex) => {
      const index = reusablePrefix.length + relativeIndex;
      const stability = phase === "complete"
        ? "stable"
        : block.closed && index < finalBlockIndex
          ? "stable"
          : block.closed && endsAtSafeBoundary(normalized.source, block.end)
            ? "stable"
            : "live";
      const syntheticClosingFence = normalized.diagnostics.some(
        (diagnostic) => diagnostic.sourceStart >= block.start && diagnostic.sourceStart <= block.end,
      );
      return {
        key: `${contentId}:${block.start}:${block.kind}`,
        source: utf8Slice(normalized.source, block.start, block.end),
        sourceStart: block.start,
        sourceEnd: block.end,
        stability,
        kind: block.kind,
        syntheticClosingFence,
      } satisfies ProjectedMarkdownBlock;
    }),
  ];
  const projection = {
    mode: phase,
    sourceLength: utf8Length(source),
    source: normalized.source,
    diagnostics: normalized.diagnostics,
    blocks: projectedBlocks,
  };
  return {
    projection,
    cursor: { contentId, phase, source, projection },
    reusedStableBlocks: reusablePrefix.length,
  };
}

function reusableStablePrefix(
  previous: MarkdownProjectionCursor | undefined,
  source: string,
  phase: MarkdownPhase,
  contentId: string,
): readonly ProjectedMarkdownBlock[] {
  if (
    previous === undefined
    || phase !== "streaming"
    || previous.phase !== "streaming"
    || previous.contentId !== contentId
    || !source.startsWith(previous.source)
  ) return [];

  const stablePrefix: ProjectedMarkdownBlock[] = [];
  for (const block of previous.projection.blocks) {
    if (block.stability !== "stable") break;
    stablePrefix.push(block);
  }
  const boundary = stablePrefix.length === 0
    ? 0
    : stablePrefix[stablePrefix.length - 1].sourceEnd;
  if (
    utf8Slice(source, 0, boundary)
    !== utf8Slice(previous.projection.source, 0, boundary)
  ) return [];
  return stablePrefix;
}

function scanBlocks(source: string): BlockSlice[] {
  if (source === "") return [];
  const lines = sourceLines(source);
  const blocks: BlockSlice[] = [];
  let markdownStart: number | undefined;
  let index = 0;

  const flushMarkdown = (end: number, closed: boolean) => {
    if (markdownStart === undefined || end <= markdownStart) return;
    const raw = utf8Slice(source, markdownStart, end);
    if (raw.trim() !== "") blocks.push({ start: markdownStart, end, kind: "markdown", closed });
    markdownStart = undefined;
  };

  while (index < lines.length) {
    const line = lines[index];
    const fence = openingFence(line.text);
    if (fence !== undefined) {
      flushMarkdown(line.start, true);
      const kind = fenceInfo(line.text).toLowerCase() === "mermaid" ? "mermaid" : "code";
      let end = line.end;
      let closed = false;
      index += 1;
      while (index < lines.length) {
        end = lines[index].end;
        if (isClosingFence(lines[index].text, fence)) {
          closed = true;
          index += 1;
          break;
        }
        index += 1;
      }
      blocks.push({ start: line.start, end, kind, closed });
      continue;
    }

    if (markdownStart === undefined) markdownStart = line.start;
    if (line.text.trim() === "") flushMarkdown(line.end, true);
    index += 1;
  }

  flushMarkdown(utf8Length(source), false);
  return blocks;
}

function sourceLines(source: string): LineSlice[] {
  const result: LineSlice[] = [];
  let byteOffset = 0;
  const parts = source.split("\n");
  for (let index = 0; index < parts.length; index += 1) {
    const text = parts[index];
    const hasNewline = index < parts.length - 1;
    const end = byteOffset + utf8Length(text) + (hasNewline ? 1 : 0);
    result.push({ text, start: byteOffset, end, hasNewline });
    byteOffset = end;
  }
  return result;
}

function fenceInfo(line: string): string {
  const match = line.match(/^ {0,3}(?:`{3,}|~{3,})\s*([A-Za-z0-9_-]*)/u);
  return match?.[1] ?? "";
}

function endsAtSafeBoundary(source: string, byteEnd: number): boolean {
  const tail = utf8Slice(source, 0, byteEnd);
  return byteEnd === utf8Length(source)
    ? /\n\s*\n$/u.test(tail) || /(?:^|\n)(?: {0,3})(?:`{3,}|~{3,})[ \t]*\n?$/u.test(tail)
    : true;
}

function utf8Slice(source: string, start: number, end: number): string {
  return new TextDecoder().decode(new TextEncoder().encode(source).slice(start, end));
}
