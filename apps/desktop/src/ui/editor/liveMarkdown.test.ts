import { Compartment, EditorState } from "@codemirror/state";
import { describe, expect, it } from "vitest";

import { liveMarkdown, liveMarkdownDecorations, liveMarkdownEnabled, markdownComposition } from "./liveMarkdown";

function stateFor(doc: string, cursor = doc.length): EditorState {
  return EditorState.create({ doc, selection: { anchor: cursor }, extensions: [liveMarkdown] });
}

function renderedRanges(state: EditorState) {
  const result: { from: number; to: number; className?: string; tagName?: string; replacement: boolean }[] = [];
  state.field(liveMarkdownDecorations).between(0, state.doc.length, (from, to, decoration) => {
    result.push({ from, to, className: decoration.spec.class, tagName: decoration.spec.tagName,
      replacement: decoration.spec.isBlock !== undefined || decoration.spec.widget !== undefined || decoration.spec.tagName === undefined && decoration.spec.class === undefined });
  });
  return result;
}

function hiddenSource(state: EditorState): string[] {
  return renderedRanges(state).filter((range) => range.from < range.to && range.replacement)
    .map((range) => state.doc.sliceString(range.from, range.to));
}

describe("live Markdown syntax presentation", () => {
  it("formats a completed bold node immediately at its closing boundary and reveals only the node being edited", () => {
    let state = stateFor("**first** and **second**", 9);
    expect(renderedRanges(state).filter((range) => range.tagName === "strong")).toHaveLength(2);
    expect(hiddenSource(state)).toEqual(["**", "**", "**", "**"]);
    state = state.update({ selection: { anchor: 4 } }).state;
    expect(hiddenSource(state)).toEqual(["**", "**"]);
    expect(state.doc.toString()).toBe("**first** and **second**");
    state = state.update({ selection: { anchor: 9 } }).state;
    expect(hiddenSource(state)).toHaveLength(4);
  });

  it("uses parsed headings, emphasis, lists, quotes, code and links while leaving fenced content literal", () => {
    const source = '# Heading\n\n- *item* and `code`\n\n> quotation\n\n[reference](https://example.com)\n\n```text\n**literal**\n```';
    const state = stateFor(source);
    const ranges = renderedRanges(state);
    expect(ranges.some((range) => range.className === "cm-md-heading cm-md-h1")).toBe(true);
    expect(ranges.some((range) => range.tagName === "em")).toBe(true);
    expect(ranges.some((range) => range.tagName === "code")).toBe(true);
    expect(ranges.some((range) => range.className === "cm-md-quote-line")).toBe(true);
    expect(ranges.some((range) => range.className === "cm-md-list-line")).toBe(true);
    expect(ranges.some((range) => range.className === "cm-md-link")).toBe(true);
    expect(ranges.filter((range) => range.className === "cm-md-code-line")).toHaveLength(3);
    expect(ranges.some((range) => range.tagName === "strong")).toBe(false);
    expect(hiddenSource(state)).toContain("](https://example.com)");
    expect(state.doc.toString()).toBe(source);
  });

  it("leaves escaped or incomplete syntax, remote images and HTML as source", () => {
    const source = '\\*literal*\n\n**unfinished\n\n![remote](https://remote.invalid/a.png)\n\n<img src="https://remote.invalid/a.png" onerror="alert(1)">';
    const state = stateFor(source);
    expect(hiddenSource(state)).toEqual([]);
    expect(renderedRanges(state)).toHaveLength(0);
    expect(state.doc.toString()).toBe(source);
  });

  it("freezes decoration changes during composition, then renders the committed document", () => {
    let state = stateFor("plain");
    state = state.update({ effects: markdownComposition.of(true) }).state;
    state = state.update({ changes: { from: 0, to: state.doc.length, insert: "**中文**" }, selection: { anchor: 6 } }).state;
    expect(renderedRanges(state)).toHaveLength(0);
    expect(state.doc.toString()).toBe("**中文**");
    state = state.update({ effects: markdownComposition.of(false) }).state;
    expect(renderedRanges(state).some((range) => range.tagName === "strong")).toBe(true);
    expect(hiddenSource(state)).toEqual(["**", "**"]);
  });

  it("changes presentation only when toggling source mode, preserving the same document and selection", () => {
    const mode = new Compartment();
    let state = EditorState.create({ doc: "**中文**", selection: { anchor: 3 }, extensions: [liveMarkdown, mode.of(liveMarkdownEnabled.of(true))] });
    const document = state.doc;
    const selection = state.selection;
    state = state.update({ effects: mode.reconfigure(liveMarkdownEnabled.of(false)) }).state;
    expect(renderedRanges(state)).toHaveLength(0);
    expect(state.doc).toBe(document);
    expect(state.selection).toBe(selection);
    state = state.update({ effects: mode.reconfigure(liveMarkdownEnabled.of(true)) }).state;
    expect(renderedRanges(state).some((range) => range.tagName === "strong")).toBe(true);
  });

  it("keeps a bounded long document intact through repeated middle edits and local presentation changes", () => {
    const source = Array.from({ length: 900 }, (_, index) =>
      `## Section ${index}\n\n- **Important** and *detail* with [reference](https://example.com/${index}).\n\n` +
      "```text\n  Literal **content** with exact whitespace  \n```\n\n").join("");
    expect(new TextEncoder().encode(source).length).toBeGreaterThan(128 * 1024);
    expect(new TextEncoder().encode(source).length).toBeLessThan(256 * 1024);
    let state = stateFor(source);
    let expected = source;
    for (let index = 0; index < 12; index += 1) {
      const position = Math.floor(expected.length / 2);
      const insert = `**中文 ${index}**`;
      expected = expected.slice(0, position) + insert + expected.slice(position);
      state = state.update({ changes: { from: position, insert }, selection: { anchor: position + insert.length } }).state;
      expect(state.doc.toString()).toBe(expected);
    }
    expect(renderedRanges(state).some((range) => range.className === "cm-md-heading cm-md-h2")).toBe(true);
  });
});
