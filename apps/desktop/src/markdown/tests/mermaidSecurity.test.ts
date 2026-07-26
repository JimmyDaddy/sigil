import { describe, expect, it } from "vitest";

import {
  admitMermaid,
  mermaidDiagramType,
  safeScopedStyle,
  sanitizeMermaidSvg,
} from "../mermaidSecurity";

describe("Mermaid admission and SVG boundary", () => {
  it("admits local diagram source and identifies its first declaration", () => {
    expect(admitMermaid("%% comment\nsequenceDiagram\nA->>B: hello")).toEqual({
      accepted: true,
      diagramType: "sequenceDiagram",
    });
    expect(mermaidDiagramType("graph TD\nA-->B")).toBe("flowchart");
  });

  it("rejects directives, controls, active content, and excessive input", () => {
    expect(admitMermaid("%%{init: {\"theme\": \"dark\"}}%%\ngraph TD").reason).toBe("directive");
    expect(admitMermaid("graph TD\u0000").reason).toBe("control_character");
    expect(admitMermaid("graph TD\nclick A call callback").reason).toBe("interaction");
    expect(admitMermaid("graph TD\nA[<b>unsafe</b>]").reason).toBe("active_content");
    expect(admitMermaid("graph TD\nA[https://example.com]").reason).toBe("active_content");
    expect(admitMermaid("graph TD\nA[image: cat]").reason).toBe("active_content");
    expect(admitMermaid(`graph TD\n${"A".repeat(32 * 1024)}`).reason).toBe("too_large");
    expect(admitMermaid(Array.from({ length: 1_001 }, () => "A").join("\n")).reason)
      .toBe("too_many_lines");
  });

  it("keeps locally scoped styles and fragment references", () => {
    const svg = [
      '<' + 'svg xmlns="http://www.w3.org/2000/svg" id="diagram-1">',
      "<style>#diagram-1 .node{fill:var(--color);clip-path:url(#clip)}</style>",
      '<defs><clipPath id="clip"><rect width="1" height="1"/></clipPath></defs>',
      '<g class="node"><a href="#clip"><text>safe</text></a></g>',
      "</" + "svg>",
    ].join("");
    const sanitized = sanitizeMermaidSvg(svg, "diagram-1", "Flowchart diagram");
    expect(sanitized).toContain("<" + "svg");
    expect(sanitized).toContain('role="img"');
    expect(sanitized).toContain('aria-label="Flowchart diagram"');
    expect(sanitized).toContain('focusable="false"');
  });

  it("removes Mermaid global keyframes while preserving scoped diagram styles", () => {
    const svg = [
      '<' + 'svg xmlns="http://www.w3.org/2000/svg" id="diagram-1">',
      "<style>",
      "#diagram-1{font-size:16px}",
      "@keyframes edge-animation-frame{from{stroke-dashoffset:0}}",
      "@keyframes dash{to{stroke-dashoffset:0}}",
      "#diagram-1 .edge-animation-fast{animation:dash 20s linear infinite}",
      "</style>",
      '<path class="edge-animation-fast" d="M0 0L1 1"/>',
      "</" + "svg>",
    ].join("");
    const sanitized = sanitizeMermaidSvg(svg, "diagram-1");
    expect(sanitized).toContain("#diagram-1");
    expect(sanitized).toContain("edge-animation-fast");
    expect(sanitized).not.toContain("@keyframes");
  });

  it("rejects escaping CSS and removes remote or event-bearing attributes", () => {
    expect(safeScopedStyle("body{background:red}", "diagram-1")).toBe(false);
    expect(safeScopedStyle("#diagram-1{fill:url(https://example.com/a)}", "diagram-1")).toBe(false);
    const unsafe = [
      '<' + 'svg xmlns="http://www.w3.org/2000/svg" id="diagram-1">',
      '<a href="https://example.com" onclick="alert(1)"><text>safe</text></a>',
      "</" + "svg>",
    ].join("");
    const sanitized = sanitizeMermaidSvg(unsafe, "diagram-1");
    expect(sanitized).not.toContain("https://");
    expect(sanitized).not.toContain("onclick");
  });

  it("rejects malformed generated keyframe blocks", () => {
    const malformed = [
      '<' + 'svg xmlns="http://www.w3.org/2000/svg" id="diagram-1">',
      "<style>@keyframes dash{to{stroke-dashoffset:0}</style>",
      "</" + "svg>",
    ].join("");
    expect(sanitizeMermaidSvg(malformed, "diagram-1")).toBeUndefined();
  });
});
