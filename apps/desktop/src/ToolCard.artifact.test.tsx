import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { ToolCard } from "./ToolCard";
import { LocaleProvider } from "./i18n";
import type { ToolArtifactPage, ToolArtifactSelector } from "./types";

afterEach(cleanup);

const artifactRef = `ta1_${"a".repeat(32)}`;
const tool = { key: "long-read", toolName: "read_file", status: "completed", text: "preview line one\npreview line two",
  artifactRef, artifactAvailability: "available" as const, artifactPersistedBytes: 60_000 };
const firstSelector: ToolArtifactSelector = { kind: "byte_slice", offset: 0, limit: 16_384 };
const secondSelector: ToolArtifactSelector = { kind: "byte_slice", offset: 16_384, limit: 16_384 };

function page(selector: ToolArtifactSelector, body: string, patch: Partial<ToolArtifactPage> = {}): ToolArtifactPage {
  return { schemaVersion: 1, requestScope: "long-output", artifactRef, selector, body, bodyEncoding: "utf8",
    returnedBytes: new TextEncoder().encode(body).length, pageSha256: `sha256:${"b".repeat(64)}`,
    artifactSha256: `sha256:${"c".repeat(64)}`, eof: true, matchCount: 0, ...patch };
}

it("labels a shortened file preview without treating its lines as the full read and retains paging and search", async () => {
  const user = userEvent.setup();
  const lines = Array.from({ length: 160 }, (_, index) => `${String(index).padStart(4, "0")} ${"x".repeat(250)}`);
  const body = lines.join("\n");
  const preview = [...lines.slice(0, 17), "[sigil: output truncated]", ...lines.slice(-16)].join("\n");
  expect(preview.split("\n")).toHaveLength(34);
  const read = vi.fn(async (selector: ToolArtifactSelector) => {
    if (selector.kind === "search_literal") return page(selector, lines[150]!, { matchCount: 1 });
    if (selector.kind !== "byte_slice") throw new Error("unexpected selector");
    const end = Math.min(body.length, selector.offset + selector.limit);
    return page(selector, body.slice(selector.offset, end), {
      eof: end === body.length,
      nextSelector: end === body.length ? undefined : { kind: "byte_slice", offset: end, limit: selector.limit },
    });
  });
  render(<ToolCard tool={{ ...tool, text: preview, artifactHasMore: true, artifactPersistedBytes: body.length }} onReadArtifact={read} />);
  expect(screen.getByText("Output preview.")).toBeTruthy();
  expect(screen.getByText("Preview")).toBeTruthy();
  expect(screen.queryByText("34 lines read.")).toBeNull();
  expect(read).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: "View full output" }));
  expect((await screen.findByLabelText("read_file saved output page")).textContent).toContain(lines[0]);
  await user.click(screen.getByRole("button", { name: "Next page" }));
  expect((await screen.findByLabelText("read_file saved output page")).textContent).toContain(lines[80]);
  await user.click(screen.getByRole("button", { name: "Next page" }));
  expect((await screen.findByLabelText("read_file saved output page")).textContent).toContain(lines[159]);
  await user.click(screen.getByRole("button", { name: "Search saved tool output" }));
  await user.type(screen.getByRole("textbox", { name: "Search saved tool output" }), "0150");
  await user.click(screen.getByRole("button", { name: "Search" }));
  expect((await screen.findByLabelText("read_file saved output search results")).textContent).toContain(lines[150]);
  expect(read).toHaveBeenLastCalledWith({ kind: "search_literal", query: "0150", startOffset: 0, maxMatches: 20, contextLines: 2 });
});

it("localizes the file preview summary and heading", () => {
  const key = "sigil.desktop.locale.v1";
  const previousLocale = window.localStorage.getItem(key);
  const previousLanguage = document.documentElement.lang;
  try {
    window.localStorage.setItem(key, "zh-CN");
    render(<LocaleProvider><ToolCard tool={tool} /></LocaleProvider>);
    expect(screen.getByText("输出预览。")).toBeTruthy();
    expect(screen.getByText("预览")).toBeTruthy();
    expect(screen.queryByText("2 lines read.")).toBeNull();
  } finally {
    cleanup();
    if (previousLocale === null) window.localStorage.removeItem(key);
    else window.localStorage.setItem(key, previousLocale);
    document.documentElement.lang = previousLanguage;
  }
});

it("keeps a completed short file read compact until requested and preserves exact text across collapse", async () => {
  const user = userEvent.setup();
  const body = "# 项目\n\n" + "读取说明与尾空格。  \n".repeat(6);
  const read = vi.fn(async (selector: ToolArtifactSelector) => page(selector, body));
  const current = { ...tool, input: "README.md", text: body, artifactPersistedBytes: new TextEncoder().encode(body).length };
  render(<ToolCard tool={current} onReadArtifact={read} />);
  expect(read).not.toHaveBeenCalled();
  expect(screen.queryByLabelText("read_file output content")).toBeNull();
  expect(screen.queryByLabelText("read_file saved output page")).toBeNull();
  expect(screen.getByLabelText("read_file input").textContent).toContain("README.md");
  await user.click(screen.getByRole("button", { name: "View full output" }));
  // The code renderer separates its final empty line with one display-only newline.
  expect((await screen.findByLabelText("read_file saved output page")).textContent).toBe(`${body}\n`);
  expect(screen.getAllByLabelText("Tool output reading area")).toHaveLength(1);
  screen.getByLabelText("Tool output reading area").scrollTop = 47;
  await user.click(screen.getByRole("button", { name: "Collapse output" }));
  expect(screen.queryByLabelText("read_file saved output page")).toBeNull();
  await user.click(screen.getByRole("button", { name: "View full output" }));
  expect(screen.getByLabelText("read_file saved output page").textContent).toBe(`${body}\n`);
  expect(screen.getByLabelText("Tool output reading area").scrollTop).toBe(47);
  expect(read).toHaveBeenCalledOnce();
});

it("replaces the preview with one bounded reading area and reuses prior pages when navigating back and forward", async () => {
  const user = userEvent.setup();
  const read = vi.fn(async (selector: ToolArtifactSelector) => selector.kind === "byte_slice" && selector.offset === 0
    ? page(selector, "first saved page", { returnedBytes: 16_384, eof: false, nextSelector: secondSelector })
    : page(selector, "last saved page", { returnedBytes: 42 }));
  render(<ToolCard tool={tool} onReadArtifact={read} />);
  expect(screen.getByLabelText("read_file output content").textContent).toContain("preview line one");
  await user.click(screen.getByRole("button", { name: "View full output" }));
  expect((await screen.findByLabelText("read_file saved output page")).textContent).toContain("first saved page");
  expect(screen.queryByLabelText("read_file output content")).toBeNull();
  expect(screen.queryByText("2 lines read.")).toBeNull();
  expect(screen.getAllByLabelText("Tool output reading area")).toHaveLength(1);
  expect(screen.getByText("Bytes 1–16384")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Previous page" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Next page" }));
  expect((await screen.findByLabelText("read_file saved output page")).textContent).toContain("last saved page");
  expect(screen.getByText("Bytes 16385–16426")).toBeTruthy();
  expect(screen.getByText("End of output")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Previous page" }));
  expect((await screen.findByLabelText("read_file saved output page")).textContent).toContain("first saved page");
  expect(read).toHaveBeenNthCalledWith(1, firstSelector);
  expect(read).toHaveBeenNthCalledWith(2, secondSelector);
  expect(read).toHaveBeenCalledTimes(2);
  expect(screen.queryByRole("button", { name: "Previous page" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Next page" }));
  expect(screen.getByLabelText("read_file saved output page").textContent).toContain("last saved page");
  expect(read).toHaveBeenCalledTimes(2);
});

it("keeps the submitted query and scan range visible, distinguishes zero matches, and restores the original page and scroll", async () => {
  const user = userEvent.setup();
  const nextSearch: ToolArtifactSelector = { kind: "search_literal", query: "needle", startOffset: 40_000, maxMatches: 20, contextLines: 2 };
  const read = vi.fn(async (selector: ToolArtifactSelector) => selector.kind === "search_literal"
    ? page(selector, "", selector.startOffset === 0 ? { eof: false, nextSelector: nextSearch } : {})
    : page(selector, selector.kind === "byte_slice" && selector.offset === 0 ? "first saved page" : "second saved page", {
      returnedBytes: 16_384, eof: false, nextSelector: secondSelector,
    }));
  render(<ToolCard tool={tool} onReadArtifact={read} />);
  await user.click(screen.getByRole("button", { name: "View full output" }));
  await screen.findByLabelText("read_file saved output page");
  await user.click(screen.getByRole("button", { name: "Next page" }));
  expect((await screen.findByLabelText("read_file saved output page")).textContent).toContain("second saved page");
  screen.getByLabelText("Tool output reading area").scrollTop = 137;
  await user.click(screen.getByRole("button", { name: "Search saved tool output" }));
  const input = screen.getByRole("textbox", { name: "Search saved tool output" });
  await user.type(input, "needle");
  await user.click(screen.getByRole("button", { name: "Search" }));
  expect(await screen.findByText("No matches in this range. Continue to search the next range.")).toBeTruthy();
  expect(screen.getByText("Results for “needle”")).toBeTruthy();
  expect(screen.getByText("Bytes 1–40000 · 0 match(es) on this page")).toBeTruthy();
  expect(screen.queryByLabelText("read_file saved output page")).toBeNull();
  await user.clear(input);
  await user.type(input, "unsubmitted edit");
  await user.keyboard("{Escape}");
  expect(screen.getByText("Results for “needle”")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Next page" }));
  expect(await screen.findByText("No more matches in this range.")).toBeTruthy();
  expect(read).toHaveBeenLastCalledWith(nextSearch);
  expect(screen.getByText("Bytes 40001–60000 · 0 match(es) on this page")).toBeTruthy();
  expect(screen.getByText("Search complete")).toBeTruthy();
  const readsBeforeSearchBack = read.mock.calls.length;
  await user.click(screen.getByRole("button", { name: "Previous page" }));
  expect(screen.getByText("Results for “needle”")).toBeTruthy();
  expect(screen.getByText("No matches in this range. Continue to search the next range.")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Next page" }));
  expect(screen.getByText("No more matches in this range.")).toBeTruthy();
  expect(read).toHaveBeenCalledTimes(readsBeforeSearchBack);
  const readsBeforeReturn = read.mock.calls.length;
  await user.click(screen.getByRole("button", { name: "Return to output" }));
  expect(screen.getByLabelText("read_file saved output page").textContent).toContain("second saved page");
  const readingArea = screen.getByLabelText("Tool output reading area");
  expect(readingArea.scrollTop).toBe(137);
  expect(document.activeElement).toBe(readingArea);
  expect(read).toHaveBeenCalledTimes(readsBeforeReturn);
  await user.click(screen.getByRole("button", { name: "Previous page" }));
  expect(screen.getByLabelText("read_file saved output page").textContent).toContain("first saved page");
  expect(read).toHaveBeenCalledTimes(readsBeforeReturn);
});

it("shows a complete zero-result search and returns to the preview when searching before the first saved page", async () => {
  const user = userEvent.setup();
  const read = vi.fn(async (selector: ToolArtifactSelector) => page(selector, ""));
  render(<ToolCard tool={tool} onReadArtifact={read} />);
  await user.click(screen.getByRole("button", { name: "Search saved tool output" }));
  await user.type(screen.getByRole("textbox", { name: "Search saved tool output" }), "absent");
  await user.click(screen.getByRole("button", { name: "Search" }));
  expect(await screen.findByText("No matches found.")).toBeTruthy();
  expect(screen.queryByText("No output.")).toBeNull();
  expect(screen.queryByLabelText("read_file output content")).toBeNull();
  await user.click(screen.getByRole("button", { name: "Return to output" }));
  expect(screen.getByLabelText("read_file output content").textContent).toContain("preview line one");
  expect(read).toHaveBeenCalledOnce();
});

it("keeps the current page and its navigation after a failed next-page read", async () => {
  const user = userEvent.setup();
  let nextReads = 0;
  const read = vi.fn(async (selector: ToolArtifactSelector) => {
    if (selector.kind === "byte_slice" && selector.offset > 0) {
      if (nextReads++ === 0) throw new Error("next page unavailable");
      return page(selector, "last saved page");
    }
    return page(selector, "first saved page", { returnedBytes: 16_384, eof: false, nextSelector: secondSelector });
  });
  render(<ToolCard tool={tool} onReadArtifact={read} />);
  await user.click(screen.getByRole("button", { name: "View full output" }));
  await screen.findByLabelText("read_file saved output page");
  await user.click(screen.getByRole("button", { name: "Next page" }));
  expect(await screen.findByRole("alert")).toBeTruthy();
  expect(screen.getByLabelText("read_file saved output page").textContent).toContain("first saved page");
  expect(screen.getByText("Bytes 1–16384")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Previous page" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Next page" }));
  expect((await screen.findByLabelText("read_file saved output page")).textContent).toContain("last saved page");
  expect(screen.queryByRole("alert")).toBeNull();
  const readsBeforeReturn = read.mock.calls.length;
  await user.click(screen.getByRole("button", { name: "Previous page" }));
  expect(screen.getByLabelText("read_file saved output page").textContent).toContain("first saved page");
  expect(read).toHaveBeenCalledTimes(readsBeforeReturn);
});

it("evicts old pages after eight cached pages while an explicit restart always reads again", async () => {
  const user = userEvent.setup();
  const read = vi.fn(async (selector: ToolArtifactSelector) => {
    const offset = selector.kind === "byte_slice" ? selector.offset : 0;
    return page(selector, `page-${offset}`, { eof: offset === 8, nextSelector: offset === 8 ? undefined
      : { kind: "byte_slice", offset: offset + 1, limit: 16_384 } });
  });
  render(<ToolCard tool={tool} onReadArtifact={read} />);
  await user.click(screen.getByRole("button", { name: "View full output" }));
  await screen.findByLabelText("read_file saved output page");
  for (let index = 1; index <= 8; index += 1) await user.click(screen.getByRole("button", { name: "Next page" }));
  expect(read).toHaveBeenCalledTimes(9);
  for (let index = 7; index >= 1; index -= 1) {
    await user.click(screen.getByRole("button", { name: "Previous page" }));
    expect(screen.getByLabelText("read_file saved output page").textContent?.trim()).toBe(`page-${index}`);
  }
  expect(read).toHaveBeenCalledTimes(9);
  await user.click(screen.getByRole("button", { name: "Previous page" }));
  expect(read).toHaveBeenCalledTimes(10);
  expect(read).toHaveBeenLastCalledWith(firstSelector);
  await user.click(screen.getByRole("button", { name: "From start" }));
  expect(read).toHaveBeenCalledTimes(11);
  expect(read).toHaveBeenLastCalledWith(firstSelector);
});

it("bounds cached bodies by actual UTF8 bytes instead of character count or returnedBytes", async () => {
  const user = userEvent.setup();
  const body = "界".repeat(6_000);
  const read = vi.fn(async (selector: ToolArtifactSelector) => {
    const offset = selector.kind === "byte_slice" ? selector.offset : 0;
    return page(selector, `${offset}:${body}`, { returnedBytes: 1, eof: offset === 3,
      nextSelector: offset === 3 ? undefined : { kind: "byte_slice", offset: offset + 1, limit: 16_384 } });
  });
  render(<ToolCard tool={tool} onReadArtifact={read} />);
  await user.click(screen.getByRole("button", { name: "View full output" }));
  await screen.findByLabelText("read_file saved output page");
  for (let index = 1; index <= 3; index += 1) await user.click(screen.getByRole("button", { name: "Next page" }));
  expect(read).toHaveBeenCalledTimes(4);
  await user.click(screen.getByRole("button", { name: "Previous page" }));
  await user.click(screen.getByRole("button", { name: "Previous page" }));
  expect(read).toHaveBeenCalledTimes(4);
  await user.click(screen.getByRole("button", { name: "Previous page" }));
  expect(read).toHaveBeenCalledTimes(5);
  expect(read).toHaveBeenLastCalledWith(firstSelector);
});

it.each(["tool key", "artifact ref", "availability"] as const)("clears cached pages when the %s changes", async (scope) => {
  const user = userEvent.setup();
  let revision = "old";
  let currentArtifactRef = artifactRef;
  const read = vi.fn(async (selector: ToolArtifactSelector) => page(selector, `${revision}-${selector.kind === "byte_slice" ? selector.offset : 0}`, {
    artifactRef: currentArtifactRef,
    eof: selector.kind === "byte_slice" && selector.offset > 0,
    nextSelector: selector.kind === "byte_slice" && selector.offset > 0 ? undefined : secondSelector,
  }));
  const view = render(<ToolCard tool={tool} onReadArtifact={read} />);
  await user.click(screen.getByRole("button", { name: "View full output" }));
  await screen.findByLabelText("read_file saved output page");
  await user.click(screen.getByRole("button", { name: "Next page" }));
  expect(read).toHaveBeenCalledTimes(2);
  revision = "new";
  const current = scope === "tool key" ? { ...tool, key: "new-tool" }
    : scope === "artifact ref" ? { ...tool, artifactRef: `ta1_${"d".repeat(32)}` } : tool;
  currentArtifactRef = current.artifactRef;
  if (scope === "availability") view.rerender(<ToolCard tool={{ ...tool, artifactAvailability: "unavailable" }} onReadArtifact={read} />);
  view.rerender(<ToolCard tool={current} onReadArtifact={read} />);
  await user.click(screen.getByRole("button", { name: "View full output" }));
  await screen.findByLabelText("read_file saved output page");
  await user.click(screen.getByRole("button", { name: "Next page" }));
  expect(read).toHaveBeenCalledTimes(4);
  expect(screen.getByLabelText("read_file saved output page").textContent).toContain("new-16384");
});

it("opens full output in one step and resumes the same page, scroll and navigation after collapsing without another read", async () => {
  const user = userEvent.setup();
  const read = vi.fn(async (selector: ToolArtifactSelector) => selector.kind === "byte_slice" && selector.offset === 0
    ? page(selector, "first saved page", { returnedBytes: 16_384, eof: false, nextSelector: secondSelector })
    : page(selector, "second saved page"));
  const view = render(<ToolCard tool={{ ...tool, text: "preview one\npreview two\npreview three\npreview four" }} onReadArtifact={read} />);
  expect(view.container.querySelector(".sg-collapsible")).toBeNull();
  expect(screen.queryByRole("button", { name: "Expand read_file output" })).toBeNull();
  const toggle = screen.getByRole("button", { name: "View full output" });
  expect(toggle.getAttribute("aria-expanded")).toBe("false");
  expect(document.getElementById(toggle.getAttribute("aria-controls")!)).toBeTruthy();
  await user.click(toggle);
  await screen.findByLabelText("read_file saved output page");
  expect(read).toHaveBeenCalledExactlyOnceWith(firstSelector);
  await user.click(screen.getByRole("button", { name: "Next page" }));
  screen.getByLabelText("Tool output reading area").scrollTop = 129;
  await user.click(screen.getByRole("button", { name: "Collapse output" }));
  expect(screen.queryByLabelText("read_file saved output page")).toBeNull();
  expect(screen.getByLabelText("read_file output content").textContent).toContain("preview one");
  await user.click(screen.getByRole("button", { name: "View full output" }));
  const readingArea = screen.getByLabelText("Tool output reading area");
  expect(screen.getByLabelText("read_file saved output page").textContent).toContain("second saved page");
  expect(readingArea.scrollTop).toBe(129);
  expect(document.activeElement).toBe(readingArea);
  expect(read).toHaveBeenCalledTimes(2);
  await user.click(screen.getByRole("button", { name: "Previous page" }));
  expect(screen.getByLabelText("read_file saved output page").textContent).toContain("first saved page");
  expect(read).toHaveBeenCalledTimes(2);
});

it("preserves results and the query across collapse, then returns a search started from the preview to that preview", async () => {
  const user = userEvent.setup();
  const read = vi.fn(async (selector: ToolArtifactSelector) => selector.kind === "search_literal"
    ? page(selector, "matching context", { matchCount: 1 }) : page(selector, "saved body"));
  render(<ToolCard tool={tool} onReadArtifact={read} />);
  await user.click(screen.getByRole("button", { name: "View full output" }));
  await screen.findByLabelText("read_file saved output page");
  screen.getByLabelText("Tool output reading area").scrollTop = 93;
  await user.click(screen.getByRole("button", { name: "Collapse output" }));
  await user.click(screen.getByRole("button", { name: "Search saved tool output" }));
  await user.type(screen.getByRole("textbox", { name: "Search saved tool output" }), "needle");
  await user.click(screen.getByRole("button", { name: "Search" }));
  await screen.findByLabelText("read_file saved output search results");
  await user.click(screen.getByRole("button", { name: "Collapse output" }));
  expect(screen.queryByRole("textbox", { name: "Search saved tool output" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "View full output" }));
  expect(screen.getByText("Results for “needle”")).toBeTruthy();
  expect(screen.getByLabelText("read_file saved output search results").textContent).toContain("matching context");
  await user.click(screen.getByRole("button", { name: "Search saved tool output" }));
  expect((screen.getByRole("textbox", { name: "Search saved tool output" }) as HTMLInputElement).value).toBe("needle");
  await user.keyboard("{Escape}");
  await user.click(screen.getByRole("button", { name: "Return to output" }));
  expect(screen.getByLabelText("read_file output content").textContent).toContain("preview line one");
  expect(screen.queryByLabelText("read_file saved output page")).toBeNull();
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "View full output" }));
  await user.click(screen.getByRole("button", { name: "View full output" }));
  expect(screen.getByLabelText("read_file saved output page").textContent).toContain("saved body");
  expect(screen.getByLabelText("Tool output reading area").scrollTop).toBe(93);
  expect(read).toHaveBeenCalledTimes(2);
});
