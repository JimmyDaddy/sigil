import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import { HistoryContent } from "./HistoryPanel";
import { LocaleProvider } from "./i18n";
import type { CatalogPage } from "./types";

afterEach(() => { cleanup(); window.localStorage.clear(); });

const page: CatalogPage = { workspaceId: "workspace", generation: 1, reconciledAtUnixMs: 1,
  degradedSourceCount: 0, identityConflictCount: 0, truncatedSourceCount: 0, entries: [] };

it("reserves no-match guidance for applied filters and does not duplicate the first-conversation prompt", () => {
  const view = (filtersApplied: boolean, state: "ready" | "error" = "ready") => <LocaleProvider><HistoryContent
    state={state} page={page} filtersApplied={filtersApplied} onRetry={vi.fn()} onLoadMore={vi.fn()}
    onOpen={vi.fn()} onRename={vi.fn()} onDelete={vi.fn()} onDeleteInvalidSource={vi.fn()} onQuarantine={vi.fn()} /></LocaleProvider>;
  const { rerender, container } = render(view(false));
  expect(screen.getByText("0 conversations")).toBeTruthy();
  expect(container.querySelector(".history-empty")).toBeNull();
  rerender(view(true));
  expect(screen.getByText("No matching conversation.")).toBeTruthy();
  expect(screen.getByText("Start a new conversation or adjust the filters.")).toBeTruthy();
  rerender(view(false, "error"));
  expect(screen.getByRole("button", { name: "Refresh conversations" })).toBeTruthy();
  expect(screen.queryByText("0 conversations")).toBeNull();
  expect(screen.queryByText("No matching conversation.")).toBeNull();
});
