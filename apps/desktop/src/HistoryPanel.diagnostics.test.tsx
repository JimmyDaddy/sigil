import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import { HistoryContent, type HistoryState } from "./HistoryPanel";
import { LocaleProvider } from "./i18n";
import type { CatalogEntry, CatalogPage } from "./types";

afterEach(() => { cleanup(); window.localStorage.clear(); });

const available: CatalogEntry = {
  sessionRef: "available.jsonl", sessionId: "available-session", sourceState: "ready",
  sourceBytes: 100, sourceModifiedAtUnixMs: 1, title: "Available conversation",
  userMessageCount: 1, assistantMessageCount: 1, toolResultCount: 0, pinned: false,
};
const page: CatalogPage = {
  workspaceId: "workspace", generation: 1, reconciledAtUnixMs: 1,
  degradedSourceCount: 1, identityConflictCount: 2, truncatedSourceCount: 3,
  entries: [available],
};

function history(state: HistoryState, value: CatalogPage = page) {
  const actions = { onRetry: vi.fn(), onLoadMore: vi.fn(), onOpen: vi.fn(), onRename: vi.fn(),
    onDelete: vi.fn(), onDeleteInvalidSource: vi.fn(), onQuarantine: vi.fn() };
  return { ...render(<LocaleProvider><HistoryContent state={state} page={value} {...actions} /></LocaleProvider>), actions };
}

it.each(["en", "zh-CN"] as const)("keeps independent catalog counts in neutral optional details in %s", (locale) => {
  window.localStorage.setItem("sigil.desktop.locale.v1", locale);
  const { container } = history("ready");
  const label = locale === "en" ? "Conversation list details" : "会话列表详情";
  const trigger = screen.getByRole("button", { name: label });
  expect(trigger.textContent).toBe(locale === "en" ? "Technical detail" : "技术详情");
  expect(screen.queryByRole("dialog", { name: label })).toBeNull();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.queryByText("6")).toBeNull();
  expect(container.querySelector(".catalog-diagnostics svg")).toBeNull();
  fireEvent.click(trigger);
  const details = screen.getByRole("dialog", { name: label });
  const counts = within(details).getAllByRole("listitem");
  expect(counts.map((row) => row.querySelector("strong")?.textContent)).toEqual(["1", "2", "3"]);
  for (const item of locale === "en"
    ? ["Unavailable sources", "Identity conflicts", "Sources outside this scan"]
    : ["不可用来源", "身份冲突", "本次未扫描来源"]) {
    expect(within(details).getByText(item)).toBeTruthy();
  }
  expect(details.querySelector("p")).toBeNull();
});

it("omits optional catalog details when all observation counts are zero", () => {
  history("ready", { ...page, degradedSourceCount: 0, identityConflictCount: 0, truncatedSourceCount: 0 });
  expect(screen.queryByRole("button", { name: "Conversation list details" })).toBeNull();
  expect(screen.getByRole("button", { name: /^Available conversation/ })).toBeTruthy();
});

it.each(["error", "stale"] as const)("keeps a real %s catalog request failure actionable", (state) => {
  const { actions } = history(state);
  expect(screen.getByRole("alert")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Refresh conversations" }));
  expect(actions.onRetry).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("button", { name: "Conversation list details" })).toBeNull();
});

it("does not let catalog observations block a ready conversation or bypass an unavailable source boundary", () => {
  const unavailable: CatalogEntry = { ...available, sessionRef: "unavailable.jsonl", sessionId: undefined,
    sourceState: "invalid", sourceDiagnostic: "unsafe_source", title: "Unavailable conversation" };
  const { actions, container } = history("ready", { ...page, entries: [available, unavailable] });
  fireEvent.click(screen.getByRole("button", { name: /^Available conversation/ }));
  expect(actions.onOpen).toHaveBeenCalledExactlyOnceWith(available);
  expect(screen.queryByRole("button", { name: /^Unavailable conversation/ })).toBeNull();
  expect(container.querySelector(".session-row-unavailable")?.getAttribute("aria-disabled")).toBe("true");
  fireEvent.contextMenu(screen.getByText("Unavailable conversation"));
  fireEvent.click(screen.getByRole("menuitem", { name: "Move unavailable source to quarantine" }));
  expect(actions.onQuarantine).toHaveBeenCalledExactlyOnceWith(unavailable);
  expect(actions.onOpen).toHaveBeenCalledTimes(1);
});
