import { describe, expect, it } from "vitest";

import { normalizeCompletedMarkdown, normalizeStreamingMarkdown } from "../normalize";

describe("bounded final markdown normalization", () => {
  it("repairs only an attached closing fence and reports its original byte range", () => {
    expect(normalizeCompletedMarkdown("```\n盒子```")).toEqual({
      source: "```\n盒子\n```",
      diagnostics: [{
        kind: "attached_closing_fence",
        sourceStart: 10,
        sourceEnd: 13,
      }],
    });
  });

  it("does not rewrite an append-only stream", () => {
    const source = "```mermaid\nflowchart TD";
    expect(normalizeStreamingMarkdown(source)).toEqual({ source, diagnostics: [] });
  });

  it("does not treat prose or a short marker as a closing fence", () => {
    const source = "````\nvalue```\ntext";
    expect(normalizeCompletedMarkdown(source)).toEqual({ source, diagnostics: [] });
  });
});
