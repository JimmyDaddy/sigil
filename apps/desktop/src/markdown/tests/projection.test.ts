import { describe, expect, it } from "vitest";

import { projectMarkdown, projectMarkdownWithCursor } from "../projection";
import { fixtureSource, markdownFixtureCases } from "./fixtures";

describe("cross-surface markdown projection corpus", () => {
  for (const testCase of markdownFixtureCases) {
    it(testCase.id, () => {
      const source = fixtureSource(testCase);
      const projection = projectMarkdown({
        source,
        phase: testCase.phase,
        contentId: testCase.id,
      });
      expect(projection.sourceLength).toBe(testCase.sourceLength);
      expect(projection.diagnostics).toEqual(testCase.diagnostics);
      expect(projection.blocks.map((block) => ({
        kind: block.kind,
        sourceStart: block.sourceStart,
        sourceEnd: block.sourceEnd,
        stability: block.stability,
        syntheticClosingFence: block.syntheticClosingFence,
      }))).toEqual(testCase.blocks);
    });
  }

  it("keeps a published stable prefix identity across append-only streaming updates", () => {
    const first = projectMarkdownWithCursor({
      source: "Stable paragraph.\n\nLive",
      phase: "streaming",
      contentId: "message-1",
    });
    const second = projectMarkdownWithCursor({
      source: "Stable paragraph.\n\nLive tail",
      phase: "streaming",
      contentId: "message-1",
    }, first.cursor);
    expect(first.projection.blocks[0]).toMatchObject({
      key: second.projection.blocks[0]?.key,
      source: second.projection.blocks[0]?.source,
      stability: "stable",
    });
    expect(second.projection.blocks[0]).toBe(first.projection.blocks[0]);
    expect(second.reusedStableBlocks).toBe(1);
    expect(first.projection.blocks[first.projection.blocks.length - 1]?.stability).toBe("live");
    expect(second.projection.blocks[second.projection.blocks.length - 1]?.stability).toBe("live");
  });

  it("invalidates block identity when a reconnect replaces the durable content id", () => {
    const before = projectMarkdownWithCursor({
      source: "Stable paragraph.\n\nLive",
      phase: "streaming",
      contentId: "provisional-message",
    });
    const reconciled = projectMarkdownWithCursor({
      source: "Stable paragraph.\n\nDurable replacement",
      phase: "complete",
      contentId: "durable-message",
    }, before.cursor);

    expect(before.projection.blocks[0]?.key).not.toBe(reconciled.projection.blocks[0]?.key);
    expect(reconciled.reusedStableBlocks).toBe(0);
    expect(reconciled.projection.blocks.every((block) => block.stability === "stable")).toBe(true);
  });

  it("fails safe to a full rebuild when a reconnect replaces non-append source", () => {
    const before = projectMarkdownWithCursor({
      source: "Stable paragraph.\n\nLive tail",
      phase: "streaming",
      contentId: "message-1",
    });
    const replacement = projectMarkdownWithCursor({
      source: "Replacement paragraph.\n\nNew tail",
      phase: "streaming",
      contentId: "message-1",
    }, before.cursor);

    expect(replacement.reusedStableBlocks).toBe(0);
    expect(replacement.projection.blocks[0]?.source).toContain("Replacement paragraph.");
  });
});
