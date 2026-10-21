import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DesktopBridge } from "../../bridge";
import { LocaleProvider } from "../../i18n";
import type { CatalogEntry, CatalogPage, SessionCatalogBatchPlan } from "../../types";
import { NotificationProvider } from "../../ui/feedback";
import { ConversationLibrary } from "./ConversationLibrary";

beforeEach(() => window.localStorage.setItem("sigil.desktop.locale.v1", "en"));
afterEach(() => { cleanup(); window.localStorage.clear(); });

const ready: CatalogEntry = {
  sessionRef: "ready.jsonl", sessionId: "ready-session", sourceState: "ready", title: "Ready conversation",
  sourceBytes: 128, sourceModifiedAtUnixMs: 1_784_419_200_000, userMessageCount: 1,
  assistantMessageCount: 1, toolResultCount: 0, pinned: false,
};
const invalid: CatalogEntry = { ...ready, sessionRef: "invalid.jsonl", sessionId: undefined,
  sourceState: "invalid", title: "Invalid source" };

function setup(blocked = false) {
  let applied = false;
  const catalog = vi.fn(async (): Promise<CatalogPage> => ({ workspaceId: "library", generation: 7,
    reconciledAtUnixMs: 1_784_419_200_000, degradedSourceCount: 1, identityConflictCount: 0,
    truncatedSourceCount: 0, entries: applied ? [] : [ready, invalid] }));
  const planSessionCatalogBatch = vi.fn(async (_workspace: string, request: Parameters<DesktopBridge["planSessionCatalogBatch"]>[1]): Promise<SessionCatalogBatchPlan> => ({
    planId: "exact-plan", action: request.action, generation: 7, total: request.items.length,
    executable: blocked ? 0 : request.items.length, blocked: blocked ? request.items.length : 0,
    items: request.items.map((item) => ({ sessionRef: item.sessionRef,
      status: blocked ? "blocked" : "executable", reason: blocked ? "pinned" : undefined })),
  }));
  const executeSessionCatalogBatch = vi.fn(async (_workspace: string, request: Parameters<DesktopBridge["executeSessionCatalogBatch"]>[1]) => {
    applied = true;
    return { planId: request.planId, action: request.action, total: request.items.length, completed: 1,
      failed: 0, skipped: 0, items: [{ sessionRef: ready.sessionRef, outcome: "completed" as const }] };
  });
  const onCatalogChanged = vi.fn(async () => undefined);
  const bridge = { catalog, planSessionCatalogBatch, executeSessionCatalogBatch } as unknown as DesktopBridge;
  render(<LocaleProvider><NotificationProvider><ConversationLibrary bridge={bridge} workspaceId="library"
    onBack={() => undefined} onOpen={() => undefined} onCatalogChanged={onCatalogChanged} />
  </NotificationProvider></LocaleProvider>);
  return { planSessionCatalogBatch, executeSessionCatalogBatch, onCatalogChanged };
}

describe("conversation library contextual batch actions", () => {
  it("hides zero-selection actions while keeping select-all available in the table", async () => {
    const user = userEvent.setup();
    setup();
    await screen.findByRole("button", { name: "Ready conversation" });
    expect(screen.queryByRole("button", { name: /Delete ready/ })).toBeNull();
    expect(document.querySelector(".library-selection-bar")).toBeNull();
    const all = screen.getByRole("checkbox", { name: "Select all loaded conversations" });
    expect(all.closest("thead")).toBeTruthy();
    await user.click(all);
    expect(screen.getByText("2 selected")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Delete ready (1)" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Quarantine unavailable (1)" })).toBeTruthy();
    await user.click(all);
    expect(document.querySelector(".library-selection-bar")).toBeNull();
  });

  it("retains the exact preview and explicit confirmation before a selected delete is applied", async () => {
    const user = userEvent.setup();
    const { planSessionCatalogBatch, executeSessionCatalogBatch, onCatalogChanged } = setup();
    await user.click(await screen.findByRole("checkbox", { name: "Select conversation: Ready conversation" }));
    await user.click(screen.getByRole("button", { name: "Delete ready (1)" }));
    await screen.findByRole("dialog", { name: "Review batch action" });
    expect(planSessionCatalogBatch).toHaveBeenCalledExactlyOnceWith("library", {
      action: "delete_sessions", items: [{ sessionRef: ready.sessionRef, sessionId: ready.sessionId }],
    });
    expect(executeSessionCatalogBatch).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Apply action" }));
    await waitFor(() => expect(executeSessionCatalogBatch).toHaveBeenCalledExactlyOnceWith("library", {
      planId: "exact-plan", action: "delete_sessions", items: [{ sessionRef: ready.sessionRef, sessionId: ready.sessionId }],
    }));
    expect(await screen.findByRole("heading", { name: "Latest batch result" })).toBeTruthy();
    await waitFor(() => expect(onCatalogChanged).toHaveBeenCalledTimes(1));
    expect(document.querySelector(".library-selection-bar")).toBeNull();
  });

  it("keeps blocked plans unexecutable and selection intact after cancelling review", async () => {
    const user = userEvent.setup();
    const { executeSessionCatalogBatch } = setup(true);
    await user.click(await screen.findByRole("checkbox", { name: "Select conversation: Ready conversation" }));
    await user.click(screen.getByRole("button", { name: "Delete ready (1)" }));
    await screen.findByRole("dialog", { name: "Review batch action" });
    expect(screen.getByRole("button", { name: "Apply action" }).hasAttribute("disabled")).toBe(true);
    expect(executeSessionCatalogBatch).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect((screen.getByRole("checkbox", { name: "Select conversation: Ready conversation" }) as HTMLInputElement).checked).toBe(true);
    expect(screen.getByRole("button", { name: "Delete ready (1)" })).toBeTruthy();
  });
});
