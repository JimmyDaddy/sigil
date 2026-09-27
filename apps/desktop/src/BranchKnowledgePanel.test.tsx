import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { BranchKnowledgePanel } from "./BranchKnowledgePanel";
import type { DesktopBridge } from "./bridge";
import type { BranchKnowledgePreview } from "./features/conversation/branchTypes";
import { LocaleProvider } from "./i18n";
import type { ConversationRecoveryCommandReceipt } from "./types";

afterEach(() => { cleanup(); localStorage.clear(); });
const source = { sourceSessionRef: "branch.jsonl", sourceSessionId: "branch-id" };
const preview: BranchKnowledgePreview = { ...source, points: [{ sourceTurnDigest: "turn", sourceMessageId: "message", sourceTextSha256: "full-hash", summarySha256: "summary-hash", summary: "The durable conclusion", truncated: true }] };
const receipt: ConversationRecoveryCommandReceipt = { commandId: "command", clientId: "desktop", sessionId: "target", action: "import_branch_knowledge", branchKnowledge: { importId: "import", alreadyImported: false }, recovery: { checkpoints: [], forkPoints: [], throughStreamSequence: 1 }, replayed: false };
function deferred<T>() { let resolve!: (value: T) => void; let reject!: (reason: Error) => void; const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
function fixture(overrides: Partial<DesktopBridge> = {}) {
  const startRun = vi.fn();
  const bridge = {
    branchLineage: async () => ({ sessionId: "target-durable", parent: { sessionRef: "parent.jsonl", sessionId: "parent", title: "Parent context", sourceTurnIndex: 0, sourceTurnDigest: "parent-turn" }, children: [{ sessionRef: "child.jsonl", sessionId: "child", title: "Child investigation", sourceTurnIndex: 1, sourceTurnDigest: "child-turn" }], unavailableCount: 1 }),
    catalog: async () => ({ workspaceId: "workspace", generation: 1, reconciledAtUnixMs: 0, degradedSourceCount: 0, identityConflictCount: 0, truncatedSourceCount: 0, entries: [{ sessionRef: source.sourceSessionRef, sessionId: source.sourceSessionId, title: "Source branch", sourceState: "ready", sourceBytes: 20, sourceModifiedAtUnixMs: 0, userMessageCount: 1, assistantMessageCount: 1, toolResultCount: 0, pinned: false }] }),
    branchKnowledgePreview: async () => preview,
    commandConversationRecovery: async () => receipt,
    startRun,
    ...overrides,
  } as DesktopBridge;
  const onImported = vi.fn();
  const onOpen = vi.fn(async () => undefined);
  const renderPanel = (sessionId: string) => <LocaleProvider><BranchKnowledgePanel bridge={bridge} workspaceId="workspace" sessionId={sessionId} onImported={onImported} onOpen={onOpen} /></LocaleProvider>;
  const rendered = render(renderPanel("target"));
  return { startRun, bridge, onImported, onOpen, ...rendered, renderPanel };
}
async function selectSource() {
  await screen.findByRole("option", { name: "Source branch" });
  await userEvent.selectOptions(screen.getByRole("combobox", { name: "Source conversation" }), JSON.stringify([source.sourceSessionRef, source.sourceSessionId]));
}

it("previews the actual conclusion, keeps failed selection, and imports six exact bindings without running", async () => {
  const pending = deferred<ConversationRecoveryCommandReceipt>();
  const command = vi.fn<DesktopBridge["commandConversationRecovery"]>().mockImplementationOnce(() => pending.promise).mockResolvedValueOnce({ ...receipt, branchKnowledge: { importId: "import", alreadyImported: true } });
  const { onImported, startRun, onOpen } = fixture({ commandConversationRecovery: command });
  await selectSource();
  expect(await screen.findByText("The durable conclusion")).toBeTruthy();
  expect(screen.getByText(/Only the displayed text/)).toBeTruthy();
  expect(command).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole("button", { name: "Import conclusion" }));
  expect(screen.getByRole("button", { name: "Importing conclusion…" })).toBeTruthy();
  expect(onImported).not.toHaveBeenCalled();
  expect(command).toHaveBeenCalledWith("workspace", { sessionId: "target", action: { kind: "import_branch_knowledge", selection: { ...source, sourceTurnDigest: "turn", sourceMessageId: "message", sourceTextSha256: "full-hash", summarySha256: "summary-hash" } } });
  await act(async () => pending.reject(new Error("source changed")));
  expect((await screen.findByRole("alert")).textContent).toContain("selection is preserved");
  expect(screen.getByText("The durable conclusion")).toBeTruthy();
  await userEvent.click(screen.getByRole("button", { name: "Import conclusion" }));
  expect(await screen.findByText("This conclusion was already imported.")).toBeTruthy();
  expect(onImported).toHaveBeenCalledOnce();
  expect(startRun).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole("button", { name: "Parent: Parent context" }));
  await userEvent.click(screen.getByRole("button", { name: "Child: Child investigation" }));
  expect(onOpen.mock.calls).toEqual([["parent.jsonl", "parent", "Parent context"], ["child.jsonl", "child", "Child investigation"]]);
});

it("does not publish a late import receipt into a replacement target", async () => {
  const pending = deferred<ConversationRecoveryCommandReceipt>();
  const { rerender, renderPanel, onImported } = fixture({ commandConversationRecovery: async () => pending.promise });
  await selectSource();
  await userEvent.click(await screen.findByRole("button", { name: "Import conclusion" }));
  rerender(renderPanel("replacement"));
  await act(async () => pending.resolve(receipt));
  await waitFor(() => expect(screen.queryByText("The durable conclusion")).toBeNull());
  expect(onImported).not.toHaveBeenCalled();
  expect(screen.queryByText(/Conclusion imported for future/)).toBeNull();
});

it("discards a preview from an earlier source selection", async () => {
  const pending = deferred<BranchKnowledgePreview>();
  fixture({ branchKnowledgePreview: async () => pending.promise });
  await selectSource();
  await userEvent.selectOptions(screen.getByRole("combobox", { name: "Source conversation" }), "");
  await act(async () => pending.resolve(preview));
  expect(screen.queryByText("The durable conclusion")).toBeNull();
  expect(screen.queryByRole("button", { name: "Import conclusion" })).toBeNull();
});
