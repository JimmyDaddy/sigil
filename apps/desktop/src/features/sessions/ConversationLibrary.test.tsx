import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DesktopBridge } from "../../bridge";
import { LocaleProvider } from "../../i18n";
import type { CatalogEntry, CatalogPage, SessionCatalogBatchPlan, SessionCatalogBatchReceipt } from "../../types";
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

function setup(blocked: boolean | ReadonlySet<string> = false, entries = [ready, invalid]) {
  let currentEntries = entries;
  let applied = false;
  const catalog = vi.fn(async (): Promise<CatalogPage> => ({ workspaceId: "library", generation: 7,
    reconciledAtUnixMs: 1_784_419_200_000, degradedSourceCount: 1, identityConflictCount: 0,
    truncatedSourceCount: 0, entries: applied ? [] : currentEntries }));
  const planSessionCatalogBatch = vi.fn(async (_workspace: string, request: Parameters<DesktopBridge["planSessionCatalogBatch"]>[1]): Promise<SessionCatalogBatchPlan> => ({
    planId: "exact-plan", action: request.action, generation: 7, total: request.items.length,
    executable: request.items.filter((item) => !(blocked === true || (blocked !== false && blocked.has(item.sessionRef)))).length,
    blocked: request.items.filter((item) => blocked === true || (blocked !== false && blocked.has(item.sessionRef))).length,
    items: request.items.map((item) => ({ sessionRef: item.sessionRef,
      status: blocked === true || (blocked !== false && blocked.has(item.sessionRef)) ? "blocked" : "executable",
      reason: blocked === true || (blocked !== false && blocked.has(item.sessionRef)) ? "pinned" : undefined })),
  }));
  const executeSessionCatalogBatch = vi.fn(async (_workspace: string, request: Parameters<DesktopBridge["executeSessionCatalogBatch"]>[1]): Promise<SessionCatalogBatchReceipt> => {
    applied = true;
    return { planId: request.planId, action: request.action, total: request.items.length, completed: 1,
      failed: 0, skipped: 0, items: [{ sessionRef: ready.sessionRef, outcome: "completed" as const }] };
  });
  const onCatalogChanged = vi.fn(async () => undefined);
  const bridge = { catalog, planSessionCatalogBatch, executeSessionCatalogBatch } as unknown as DesktopBridge;
  render(<LocaleProvider><NotificationProvider><ConversationLibrary bridge={bridge} workspaceId="library"
    onBack={() => undefined} onOpen={() => undefined} onCatalogChanged={onCatalogChanged} />
  </NotificationProvider></LocaleProvider>);
  return { catalog, planSessionCatalogBatch, executeSessionCatalogBatch, onCatalogChanged,
    replaceEntries: (next: CatalogEntry[]) => { currentEntries = next; } };
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
    await screen.findByRole("alertdialog", { name: "Delete conversations (1)?" });
    expect(planSessionCatalogBatch).toHaveBeenCalledExactlyOnceWith("library", {
      action: "delete_sessions", items: [{ sessionRef: ready.sessionRef, sessionId: ready.sessionId }],
    });
    const dialog = screen.getByRole("alertdialog");
    expect(within(dialog).getByText("Ready conversation")).toBeTruthy();
    expect(within(dialog).getByText(/permanently removes the local conversation history/)).toBeTruthy();
    expect(within(dialog).getByText("Will be processed")).toBeTruthy();
    expect(executeSessionCatalogBatch).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cancel" }));
    await user.click(screen.getByRole("button", { name: "Permanently delete conversations (1)" }));
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
    await screen.findByRole("alertdialog", { name: "Delete conversations (1)?" });
    expect(screen.getByRole("button", { name: "Permanently delete conversations (0)" }).hasAttribute("disabled")).toBe(true);
    expect(executeSessionCatalogBatch).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect((screen.getByRole("checkbox", { name: "Select conversation: Ready conversation" }) as HTMLInputElement).checked).toBe(true);
    expect(screen.getByRole("button", { name: "Delete ready (1)" })).toBeTruthy();
  });
});

it("puts existing filters before records without a duplicate page introduction and keeps all filter actions", async () => {
  const user = userEvent.setup();
  const { catalog } = setup();
  await screen.findByRole("button", { name: "Ready conversation" });
  expect(screen.getAllByRole("heading", { name: "Conversation Library" })).toHaveLength(1);
  expect(document.querySelector(".application-page-intro")).toBeNull();
  expect(document.querySelector(".application-page-content")?.firstElementChild).toHaveProperty("className", "library-command-surface");
  await user.type(screen.getByRole("textbox", { name: "Provider" }), "exact-provider");
  await user.selectOptions(screen.getByRole("combobox", { name: "Source state" }), "invalid");
  await user.click(screen.getByRole("checkbox", { name: "Pinned only" }));
  await user.type(screen.getByRole("textbox", { name: "Search conversations" }), "needle");
  await user.click(screen.getByRole("button", { name: "Search" }));
  await waitFor(() => expect(catalog).toHaveBeenLastCalledWith("library", expect.objectContaining({ query: "needle", provider: "exact-provider", state: "invalid", pinned: true })));
});


describe("batch review target and action clarity", () => {
  it.each([
    ["Quarantine unavailable (1)", "Quarantine unavailable sources (1)?", "Quarantine sources (1)", "quarantine_invalid_sources", /kept in the local quarantine directory/],
    ["Delete unavailable (1)", "Delete unavailable sources (1)?", "Permanently delete sources (1)", "delete_invalid_sources", /cannot be recovered from Sigil's quarantine directory/],
  ] as const)("identifies %s and retains exact source binding", async (trigger, title, confirmation, action, impact) => {
    const user = userEvent.setup();
    const { executeSessionCatalogBatch } = setup();
    await user.click(await screen.findByRole("checkbox", { name: "Select conversation: Invalid source" }));
    await user.click(screen.getByRole("button", { name: trigger }));
    const dialog = await screen.findByRole("alertdialog", { name: title });
    expect(within(dialog).getByText("Invalid source")).toBeTruthy();
    expect(within(dialog).getByText(impact)).toBeTruthy();
    await user.click(within(dialog).getByRole("button", { name: confirmation }));
    await waitFor(() => expect(executeSessionCatalogBatch).toHaveBeenCalledExactlyOnceWith("library", {
      planId: "exact-plan", action,
      items: [{ sessionRef: invalid.sessionRef, sourceBytes: invalid.sourceBytes, sourceModifiedAtUnixMs: invalid.sourceModifiedAtUnixMs }],
    }));
  });

  it("lists executable, blocked, same-name and untitled targets and distinguishes their identities", async () => {
    const user = userEvent.setup();
    const other = { ...ready, sessionRef: "same-name.jsonl", sessionId: "other", title: ready.title };
    const untitled = { ...ready, sessionRef: "untitled.jsonl", sessionId: "untitled", title: undefined };
    setup(new Set([other.sessionRef]), [ready, other, untitled]);
    await user.click(await screen.findByRole("checkbox", { name: "Select all loaded conversations" }));
    await user.click(screen.getByRole("button", { name: "Delete ready (3)" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Delete conversations (3)?" });
    const targets = within(dialog).getByRole("list", { name: "Selected targets" });
    expect(within(targets).getAllByRole("listitem")).toHaveLength(3);
    expect(within(targets).getAllByText("Ready conversation")).toHaveLength(2);
    expect(within(targets).getByText("Untitled conversation")).toBeTruthy();
    expect(within(targets).getByText("Pinned")).toBeTruthy();
    for (const ref of [ready.sessionRef, other.sessionRef, untitled.sessionRef]) {
      expect(within(targets).getByText(ref).closest("details")).toBeNull();
    }
    expect(within(dialog).getByRole("button", { name: "Permanently delete conversations (2)" }).hasAttribute("disabled")).toBe(false);
  });

  it("keeps preview labels and exact items stable when the catalog refreshes during planning", async () => {
    const user = userEvent.setup();
    const { replaceEntries, planSessionCatalogBatch, executeSessionCatalogBatch } = setup();
    let resolvePlan!: (plan: SessionCatalogBatchPlan) => void;
    planSessionCatalogBatch.mockImplementationOnce(() => new Promise((resolve) => { resolvePlan = resolve; }));
    await user.click(await screen.findByRole("checkbox", { name: "Select conversation: Ready conversation" }));
    await user.click(screen.getByRole("button", { name: "Delete ready (1)" }));
    replaceEntries([{ ...ready, title: "Renamed after preview", sessionId: "changed-session" }]);
    await user.type(screen.getByRole("textbox", { name: "Provider" }), "changed");
    await screen.findByRole("button", { name: "Renamed after preview" });
    resolvePlan({ planId: "frozen-plan", action: "delete_sessions", generation: 7, total: 1,
      executable: 1, blocked: 0, items: [{ sessionRef: ready.sessionRef, status: "executable" }] });
    const dialog = await screen.findByRole("alertdialog", { name: "Delete conversations (1)?" });
    expect(within(dialog).getByText("Ready conversation")).toBeTruthy();
    expect(within(dialog).queryByText("Renamed after preview")).toBeNull();
    await user.click(within(dialog).getByRole("button", { name: "Permanently delete conversations (1)" }));
    await waitFor(() => expect(executeSessionCatalogBatch).toHaveBeenCalledExactlyOnceWith("library", {
      planId: "frozen-plan", action: "delete_sessions", items: [{ sessionRef: ready.sessionRef, sessionId: ready.sessionId }],
    }));
  });

  it("shows activity units and keeps completed receipts available on demand", async () => {
    const user = userEvent.setup();
    setup();
    await screen.findByRole("button", { name: "Ready conversation" });
    expect(screen.getAllByText("2 messages", { exact: false }).length).toBeGreaterThan(0);
    expect(screen.getAllByText("0 tool results", { exact: false }).length).toBeGreaterThan(0);
    await user.click(screen.getByRole("checkbox", { name: "Select conversation: Ready conversation" }));
    await user.click(screen.getByRole("button", { name: "Delete ready (1)" }));
    await user.click(await screen.findByRole("button", { name: "Permanently delete conversations (1)" }));
    const details = (await screen.findByText("View individual results")).closest("details")!;
    expect(details.open).toBe(false);
    await user.click(within(details).getByText("View individual results"));
    expect(within(details).getByText("Completed")).toBeTruthy();
    expect(within(details).getByText(ready.sessionRef)).toBeTruthy();
  });

  it("localizes individual outcome labels while retaining all result identities", async () => {
    window.localStorage.setItem("sigil.desktop.locale.v1", "zh-CN");
    const user = userEvent.setup();
    const { executeSessionCatalogBatch } = setup();
    executeSessionCatalogBatch.mockResolvedValueOnce({ planId: "exact-plan", action: "delete_sessions", total: 3,
      completed: 1, failed: 1, skipped: 1, items: [
        { sessionRef: "completed.jsonl", outcome: "completed" },
        { sessionRef: "failed.jsonl", outcome: "failed", reason: "writer_busy" },
        { sessionRef: "skipped.jsonl", outcome: "skipped", reason: "identity_changed" },
      ] });
    await user.click(await screen.findByRole("checkbox", { name: "选择会话: Ready conversation" }));
    await user.click(screen.getByRole("button", { name: "删除可用会话（1）" }));
    await user.click(await screen.findByRole("button", { name: "永久删除 1 个会话" }));
    const details = (await screen.findByText("查看逐项处理结果")).closest("details")!;
    expect(details.open).toBe(false);
    await user.click(within(details).getByText("查看逐项处理结果"));
    expect(within(details).getByText("已完成")).toBeTruthy();
    expect(within(details).getByText(/^失败 · /)).toBeTruthy();
    expect(within(details).getByText(/^已跳过 · /)).toBeTruthy();
    for (const ref of ["completed.jsonl", "failed.jsonl", "skipped.jsonl"]) {
      expect(within(details).getByText(ref)).toBeTruthy();
    }
  });
});
