import DOMPurify from "dompurify";

export const MAX_MERMAID_BYTES = 32 * 1024;
export const MAX_MERMAID_LINES = 1_000;
export const MAX_MERMAID_DIAGRAMS_PER_MESSAGE = 16;

const BLOCKED_DIRECTIVE = /^\s*%%\s*\{/mu;
const BLOCKED_CONTROL_CHARACTER = /[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/u;
const BLOCKED_INTERACTION = /^\s*click\s+\S+/imu;
const BLOCKED_ACTIVE_CONTENT =
  /(?:<\/?[a-z][^>]*>|\b(?:https?|file|data|javascript):|\bforeignobject\b|\bimage\s*:)/iu;
const BLOCKED_CSS_TOKEN = /@import|expression\s*\(|behavior\s*:|-moz-binding|javascript:|data:/iu;
const CSS_URL = /url\(([^)]+)\)/giu;

export interface MermaidAdmission {
  readonly accepted: boolean;
  readonly reason?:
    | "too_large"
    | "too_many_lines"
    | "control_character"
    | "directive"
    | "interaction"
    | "active_content";
  readonly diagramType: string;
}

export function admitMermaid(source: string): MermaidAdmission {
  const diagramType = mermaidDiagramType(source);
  if (new TextEncoder().encode(source).byteLength > MAX_MERMAID_BYTES) {
    return { accepted: false, reason: "too_large", diagramType };
  }
  if (source.split("\n").length > MAX_MERMAID_LINES) {
    return { accepted: false, reason: "too_many_lines", diagramType };
  }
  if (BLOCKED_CONTROL_CHARACTER.test(source)) {
    return { accepted: false, reason: "control_character", diagramType };
  }
  if (BLOCKED_DIRECTIVE.test(source)) {
    return { accepted: false, reason: "directive", diagramType };
  }
  if (BLOCKED_INTERACTION.test(source)) {
    return { accepted: false, reason: "interaction", diagramType };
  }
  if (BLOCKED_ACTIVE_CONTENT.test(source)) {
    return { accepted: false, reason: "active_content", diagramType };
  }
  return { accepted: true, diagramType };
}

export function mermaidDiagramType(source: string): string {
  for (const rawLine of source.split("\n")) {
    const line = rawLine.trim();
    if (line === "" || line.startsWith("%%")) continue;
    const token = line.split(/\s+/u)[0]?.replace(/[^A-Za-z0-9_-]/gu, "") ?? "";
    return token === "graph" ? "flowchart" : token || "mermaid";
  }
  return "mermaid";
}

export function sanitizeMermaidSvg(
  svg: string,
  expectedRootId: string,
  accessibleLabel = "Mermaid diagram",
): string | undefined {
  const sanitized = DOMPurify.sanitize(svg, {
    USE_PROFILES: { svg: true, svgFilters: true },
    FORBID_TAGS: ["script", "foreignObject", "iframe", "object", "embed", "image"],
    FORBID_ATTR: ["onload", "onclick", "onerror", "onmouseover", "onfocus", "src"],
  });
  const documentNode = new DOMParser().parseFromString(sanitized, "image/svg+xml");
  if (documentNode.querySelector("parsererror") !== null) return undefined;
  const root = documentNode.documentElement;
  if (root.localName !== "svg" || root.getAttribute("id") !== expectedRootId) return undefined;
  root.setAttribute("role", "img");
  root.setAttribute("aria-label", accessibleLabel.slice(0, 160));
  root.setAttribute("focusable", "false");

  for (const element of Array.from(root.querySelectorAll("*"))) {
    for (const attribute of Array.from(element.attributes)) {
      const name = attribute.name.toLowerCase();
      const value = attribute.value.trim();
      if (name.startsWith("on")) element.removeAttribute(attribute.name);
      if ((name === "href" || name === "xlink:href") && !isLocalFragment(value)) {
        element.removeAttribute(attribute.name);
      }
      if (name === "style" && !safeCssDeclaration(value)) return undefined;
    }
  }

  for (const style of Array.from(root.querySelectorAll("style"))) {
    const scopedCss = stripKeyframeRules(style.textContent ?? "");
    if (scopedCss === undefined || !safeScopedStyle(scopedCss, expectedRootId)) return undefined;
    style.textContent = scopedCss;
  }
  return new XMLSerializer().serializeToString(root);
}

export function safeScopedStyle(css: string, expectedRootId: string): boolean {
  if (BLOCKED_CSS_TOKEN.test(css) || !safeCssUrls(css)) return false;
  const withoutDeclarations = css.replace(/\/\*[\s\S]*?\*\//gu, "");
  const blocks = withoutDeclarations.matchAll(/([^{}]+)\{([^{}]*)\}/gu);
  let blockCount = 0;
  for (const match of blocks) {
    blockCount += 1;
    const selectors = match[1].split(",").map((selector) => selector.trim());
    if (
      selectors.some((selector) =>
        selector === ""
        || selector.startsWith("@")
        || !(selector === `#${expectedRootId}` || selector.startsWith(`#${expectedRootId} `))
      )
    ) return false;
    if (!safeCssDeclaration(match[2])) return false;
  }
  return blockCount > 0 || css.trim() === "";
}

function stripKeyframeRules(css: string): string | undefined {
  const keyframes = /@(?:-webkit-)?keyframes\b/giu;
  let output = "";
  let cursor = 0;
  let match = keyframes.exec(css);
  while (match !== null) {
    output += css.slice(cursor, match.index);
    const openingBrace = css.indexOf("{", keyframes.lastIndex);
    if (openingBrace === -1) return undefined;
    const header = css.slice(keyframes.lastIndex, openingBrace).trim();
    if (!/^[A-Za-z_][A-Za-z0-9_-]*$/u.test(header)) return undefined;
    let depth = 1;
    let index = openingBrace + 1;
    for (; index < css.length && depth > 0; index += 1) {
      if (css[index] === "{") depth += 1;
      if (css[index] === "}") depth -= 1;
    }
    if (depth !== 0) return undefined;
    cursor = index;
    keyframes.lastIndex = cursor;
    match = keyframes.exec(css);
  }
  return output + css.slice(cursor);
}

function safeCssDeclaration(css: string): boolean {
  return !BLOCKED_CSS_TOKEN.test(css) && safeCssUrls(css);
}

function safeCssUrls(css: string): boolean {
  for (const match of css.matchAll(CSS_URL)) {
    const target = match[1].trim().replace(/^['"]|['"]$/gu, "");
    if (!isLocalFragment(target)) return false;
  }
  return true;
}

function isLocalFragment(value: string): boolean {
  return /^#[A-Za-z_][A-Za-z0-9_.:-]*$/u.test(value);
}
