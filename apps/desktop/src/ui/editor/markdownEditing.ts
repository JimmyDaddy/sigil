export type MarkdownFormat = "bold" | "italic" | "code" | "code_block" | "list" | "link";

export interface MarkdownEdit {
  readonly from: number;
  readonly to: number;
  readonly insert: string;
  readonly anchor: number;
  readonly head: number;
}

export function formatMarkdown(source: string, from: number, to: number, format: MarkdownFormat): MarkdownEdit {
  const selected = source.slice(from, to);
  if (format === "list") {
    const start = from === 0 ? 0 : source.lastIndexOf("\n", from - 1) + 1;
    const lastSelected = to > from && source[to - 1] === "\n" ? to - 1 : to;
    const nextBreak = source.indexOf("\n", lastSelected);
    const end = nextBreak < 0 ? source.length : nextBreak;
    const lines = source.slice(start, end).split("\n");
    const remove = lines.every((line) => line.startsWith("- "));
    const insert = lines.map((line) => remove ? line.slice(2) : `- ${line}`).join("\n");
    return { from: start, to: end, insert, anchor: start, head: start + insert.length };
  }
  if (format === "code_block") {
    const fence = "`".repeat(Math.max(3, longestBackticks(selected) + 1));
    const prefix = from > 0 && source[from - 1] !== "\n" ? "\n" : "";
    const suffix = to < source.length && source[to] !== "\n" ? "\n" : "";
    const opening = `${prefix}${fence}\n`;
    return { from, to, insert: `${opening}${selected}\n${fence}${suffix}`,
      anchor: from + opening.length, head: from + opening.length + selected.length };
  }
  if (format === "link") {
    const insert = `[${selected}](https://)`;
    const anchor = from + selected.length + 3;
    return { from, to, insert, anchor, head: anchor + "https://".length };
  }
  const marker = format === "bold" ? "**" : format === "italic" ? "*" : "`".repeat(longestBackticks(selected) + 1);
  if (format !== "code" && selected.startsWith(marker) && selected.endsWith(marker)
    && selected.length >= marker.length * 2) {
    const insert = selected.slice(marker.length, -marker.length);
    return { from, to, insert, anchor: from, head: from + insert.length };
  }
  const padding = format === "code" && (selected.startsWith("`") || selected.endsWith("`")) ? " " : "";
  const opening = `${marker}${padding}`;
  return { from, to, insert: `${opening}${selected}${padding}${marker}`,
    anchor: from + opening.length, head: from + opening.length + selected.length };
}

function longestBackticks(text: string): number {
  let longest = 0;
  for (const match of text.matchAll(/`+/gu)) longest = Math.max(longest, match[0].length);
  return longest;
}
