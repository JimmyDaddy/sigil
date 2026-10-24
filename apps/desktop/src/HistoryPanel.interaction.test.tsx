import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { HistoryContent } from "./HistoryPanel";
import { LocaleProvider } from "./i18n";
import type { CatalogEntry, CatalogPage } from "./types";

afterEach(cleanup);
const entry: CatalogEntry = { sessionRef: "exact.jsonl", sessionId: "exact-session", title: "Project notes",
  sourceState: "ready", sourceBytes: 10, sourceModifiedAtUnixMs: 1, userMessageCount: 1,
  assistantMessageCount: 1, toolResultCount: 0, pinned: false };
function setup() {
  const page: CatalogPage = { workspaceId: "history", generation: 1, reconciledAtUnixMs: 1,
    degradedSourceCount: 0, identityConflictCount: 0, truncatedSourceCount: 0, entries: [entry] };
  const actions = { onRetry: vi.fn(), onLoadMore: vi.fn(), onOpen: vi.fn(), onRename: vi.fn(),
    onDelete: vi.fn(), onDeleteInvalidSource: vi.fn(), onQuarantine: vi.fn() };
  render(<LocaleProvider><HistoryContent state="ready" page={page} {...actions} /></LocaleProvider>);
  return actions;
}

it("opens on the first click and does not reinterpret a double click as rename", async () => {
  const user = userEvent.setup();
  const actions = setup();
  await user.dblClick(screen.getByRole("button", { name: /^Project notes/ }));
  expect(actions.onOpen).toHaveBeenCalledExactlyOnceWith(entry);
  expect(actions.onRename).not.toHaveBeenCalled();
});

it("keeps keyboard opening and explicit menu renaming bound to the exact conversation", async () => {
  const user = userEvent.setup();
  const actions = setup();
  screen.getByRole("button", { name: /^Project notes/ }).focus();
  await user.keyboard("{Enter}");
  expect(actions.onOpen).toHaveBeenCalledExactlyOnceWith(entry);
  const menu = screen.getByRole("button", { name: "Manage Project notes" });
  menu.focus();
  await user.keyboard("{Enter}");
  const rename = screen.getByRole("menuitem", { name: "Rename" });
  rename.focus();
  await user.keyboard("{Enter}");
  expect(actions.onRename).toHaveBeenCalledExactlyOnceWith(entry);
  expect(actions.onOpen).toHaveBeenCalledOnce();
});
