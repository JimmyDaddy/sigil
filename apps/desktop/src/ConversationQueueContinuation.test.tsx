import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ConversationPanel } from "./ConversationPanel";
import type { DesktopBridge } from "./bridge";
import { LocaleProvider } from "./i18n";
import { editorText, setEditorText } from "./test/editor";
import type { ConversationDisplayPage, ConversationQueueView, RunSummary, SessionSummary, TimelineEvent } from "./types";
import { NotificationProvider } from "./ui/feedback";
import { createCatalogWorkbenchBridge } from "./ui/catalog/workbenchBridge";

afterEach(() => { cleanup(); window.localStorage.clear(); vi.restoreAllMocks(); });

const workspaceId = "workspace-queue-continuation";
const session: SessionSummary = { id: "http-queue-current", label: "Queue conversation", runCount: 1 };
const predecessor = "run-cancelled-predecessor";
const successor = "run-actual-queued-successor";
const draft = "# Main draft stays separate\n\n" + "保留正文、公式 $x^2$ 与尾空格。  \n".repeat(160);
const answer = "The resumed follow-up read the workspace and completed.";
const ownerRevision = `sha256:${"b".repeat(64)}`;

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((accept, decline) => { resolve = accept; reject = decline; });
  return { promise, resolve, reject };
}

function harness(mode: "live" | "before-receipt" | "after-receipt", initialLive = true) {
  let owner: string | undefined = initialLive ? predecessor : undefined;
  let cancelled = !initialLive;
  let completed = false;
  let paused = !initialLive;
  let promoted = false;
  let generation = 0;
  let listener: ((event: TimelineEvent) => void) | undefined;
  const run = (id: string, sessionId: string, status: RunSummary["status"]): RunSummary => ({
    id, sessionId, status, permissionMode: "manual", streamSequence: 0,
  });
  const queue = (sessionId: string): ConversationQueueView => ({
    schemaVersion: 1, sessionId, generation: String(generation), paused,
    totalItems: promoted ? 0 : 1, truncated: false,
    items: promoted ? [] : [{ entryId: "exact-queued-entry", order: 0, kind: "chat", status: "queued",
      promptPreview: "Read README.md", promptPreviewTruncated: false, promptMaterial: "persisted_safe",
      dispatchable: !paused && owner === undefined }],
  });
  const page = (sessionId: string): ConversationDisplayPage => ({
    schemaVersion: 1, requestScope: sessionId, throughSessionStreamSequence: completed ? "6" : cancelled ? "1" : "0",
    totalItems: completed ? "3" : cancelled ? "1" : "0", hasMore: false, gapFacts: [],
    items: [
      ...(cancelled ? [{ schemaVersion: 1 as const, displayId: "cancelled-terminal", displayOrder: { sessionStreamSequence: "1", subindex: 0 },
        sourceEventId: "cancelled-terminal-event", kind: "terminal" as const, source: "durable_run_event" as const,
        runId: predecessor, status: "cancelled" as const, content: { type: "terminal" as const, summaryTruncated: false } }] : []),
      ...(completed ? [{ schemaVersion: 1 as const, displayId: "actual-successor-answer", displayOrder: { sessionStreamSequence: "5", subindex: 0 },
        sourceEventId: "actual-answer-event", kind: "assistant_message" as const, source: "durable_transcript" as const,
        runId: successor, status: "succeeded" as const, content: { type: "message" as const, role: "assistant" as const,
          assistantPhase: "final_answer" as const, text: answer, imageAttachmentCount: 0, truncated: false, originalContentBytes: answer.length } },
      { schemaVersion: 1 as const, displayId: "actual-successor-terminal", displayOrder: { sessionStreamSequence: "6", subindex: 0 },
        sourceEventId: "actual-terminal-event", kind: "terminal" as const, source: "durable_run_event" as const,
        runId: successor, status: "succeeded" as const, content: { type: "terminal" as const,
          finalMessageId: "actual-successor-answer", summaryTruncated: false } }] : []),
    ],
    terminalFrontier: completed ? { runId: successor, sessionStreamSequence: "6", status: "succeeded" }
      : cancelled ? { runId: predecessor, sessionStreamSequence: "1", status: "cancelled" } : undefined,
  });
  const continuity = vi.fn<DesktopBridge["continuity"]>(async (_workspace, sessionId) => ({
    durableFrontier: { throughStreamSequence: completed ? 6 : cancelled ? 1 : 0 },
    foregroundOwner: sessionId === session.id && owner !== undefined ? { runId: owner, ownerRevision } : undefined,
    retainedTerminalRuns: [], recoveryActions: [],
  }));
  const attachRun = vi.fn<DesktopBridge["attachRun"]>(async (_workspace, input) => ({
    run: run(input.runId, input.sessionId, "running"), events: [], streamState: "live", hasGap: false,
  }));
  const display = vi.fn<DesktopBridge["display"]>(async (_workspace, sessionId) => sessionId === session.id ? page(sessionId) : {
    ...page(sessionId), throughSessionStreamSequence: "0", totalItems: "0", items: [], terminalFrontier: undefined,
  });
  const startRun = vi.fn<DesktopBridge["startRun"]>();
  const commandConversationQueue = vi.fn<DesktopBridge["commandConversationQueue"]>(async (_workspace, input) => {
    generation += 1;
    if (input.action.action === "pause") paused = true;
    if (input.action.action === "resume") {
      paused = false;
      if (mode === "live") { promoted = true; owner = successor; }
      if (mode === "before-receipt") { promoted = true; completed = true; owner = undefined; }
    }
    const receipt = { commandId: `queue-command-${generation}`, clientId: "test", sessionId: input.sessionId,
      action: input.action.action, expectedGeneration: input.expectedGeneration, generation: String(generation),
      queue: queue(input.sessionId), replayed: false };
    if (input.action.action === "resume" && mode === "after-receipt") {
      queueMicrotask(() => { promoted = true; completed = true; owner = undefined; });
    }
    return receipt;
  });
  const bridge: DesktopBridge = {
    ...createCatalogWorkbenchBridge("sigil_dark"), continuity, display, attachRun, startRun, commandConversationQueue,
    conversationQueue: async (_workspace, sessionId) => sessionId === session.id ? queue(sessionId)
      : { ...queue(sessionId), totalItems: 0, items: [], paused: false },
    subscribeRunEvents: async (next) => { listener = next; return () => { if (listener === next) listener = undefined; }; },
    subscribeRunStreamStatus: async () => () => undefined,
    cancelRun: async (_workspace, sessionId, runId) => {
      cancelled = true; owner = undefined;
      listener?.({ workspaceId, sessionId, runId, sequence: 1, runSequence: "1", kind: "run_cancelled", replayable: true });
      return run(runId, sessionId, "cancelled");
    },
  };
  const emitLiveAnswer = () => {
    expect(attachRun.mock.calls.some(([, input]) => input.runId === successor)).toBe(true);
    listener?.({ workspaceId, sessionId: session.id, runId: successor, sequence: 1, runSequence: "1",
      kind: "run_started", provisionalId: "successor-live-user", text: "Read README.md", replayable: true });
    listener?.({ workspaceId, sessionId: session.id, runId: successor, sequence: 2, runSequence: "2",
      kind: "assistant_message", provisionalId: "successor-live-answer", assistantKind: "final_answer", text: answer, replayable: true });
  };
  return { bridge, continuity, attachRun, display, startRun, commandConversationQueue, emitLiveAnswer, queue };
}

function panel(bridge: DesktopBridge, selected = session) {
  return <LocaleProvider><NotificationProvider><ConversationPanel key={selected.id} bridge={bridge}
    workspaceId={workspaceId} session={selected} draftSessionId={`durable-${selected.id}`}
    onNewSession={async () => false} onOpenWorkspacePicker={() => undefined} onOpenSessionPicker={() => undefined}
    onOpenSettings={() => undefined} onOpenSupport={() => undefined} onOpenFork={async () => undefined} />
  </NotificationProvider></LocaleProvider>;
}

async function composer() {
  const editor = await screen.findByRole("combobox", { name: "Message Sigil" });
  await waitFor(() => expect(editor.getAttribute("aria-disabled")).not.toBe("true"));
  return editor;
}

async function settledLifecycle(lifecycle: "idle" | "live") {
  await waitFor(() => {
    const panel = document.querySelector(".conversation-panel")!;
    expect(panel.getAttribute("data-continuity-lifecycle")).toBe(lifecycle);
    expect(panel.getAttribute("data-continuity-refresh-state")).toBe("idle");
    expect(panel.hasAttribute("data-continuity-pending-terminal-run-id")).toBe(false);
  }, { timeout: 4_000 });
}

describe("resuming the follow-up queue", () => {
  it.each(["live", "before-receipt", "after-receipt"] as const)(
    "follows the actual successor after Pause, Cancel and Resume when it is %s",
    async (mode) => {
      const user = userEvent.setup();
      const test = harness(mode);
      render(panel(test.bridge));
      await screen.findByRole("button", { name: "Cancel work" });
      await settledLifecycle("live");
      setEditorText(await composer(), draft);
      await user.click(await screen.findByRole("button", { name: "Open follow-up queue, 1 messages" }));
      await user.click(screen.getByRole("button", { name: "Pause queue" }));
      await screen.findByRole("button", { name: "Resume queue" });
      await user.keyboard("{Escape}");
      await user.click(screen.getByRole("button", { name: "Cancel work" }));
      await waitFor(() => expect(screen.queryByRole("button", { name: "Cancel work" })).toBeNull());
      await settledLifecycle("idle");
      await composer();
      await user.click(screen.getByRole("button", { name: "Open follow-up queue, 1 messages" }));
      await user.click(screen.getByRole("button", { name: "Resume queue" }));
      if (mode === "live") {
        await waitFor(() => expect(test.attachRun).toHaveBeenCalledWith(workspaceId, {
          sessionId: session.id, runId: successor, ownerRevision,
        }));
        await settledLifecycle("live");
        act(() => test.emitLiveAnswer());
      }
      expect(await screen.findByText(answer, {}, { timeout: 4_000 })).toBeTruthy();
      expect(screen.getAllByText(answer)).toHaveLength(1);
      expect(test.commandConversationQueue.mock.calls.map(([, input]) => input.action.action)).toEqual(["pause", "resume"]);
      expect(test.startRun).not.toHaveBeenCalled();
      expect(editorText(await composer())).toBe(draft);
      if (mode !== "live") {
        expect(test.attachRun.mock.calls.some(([, input]) => input.runId === successor)).toBe(false);
        expect(test.display.mock.calls.length).toBeGreaterThanOrEqual(3);
        expect(screen.queryByRole("button", { name: "Cancel work" })).toBeNull();
      }
    }, 12_000,
  );

  it("keeps a pending or uncertain Resume from reporting a successor or admitting a duplicate command", async () => {
    const user = userEvent.setup();
    const test = harness("live", false);
    const pending = deferred<Awaited<ReturnType<DesktopBridge["commandConversationQueue"]>>>();
    const command = vi.fn<DesktopBridge["commandConversationQueue"]>(() => pending.promise);
    render(panel({ ...test.bridge, commandConversationQueue: command }));
    setEditorText(await composer(), draft);
    await settledLifecycle("idle");
    await user.click(await screen.findByRole("button", { name: "Open follow-up queue, 1 messages" }));
    const resume = screen.getByRole("button", { name: "Resume queue" });
    const probeCount = test.continuity.mock.calls.length;
    await user.click(resume);
    expect(resume.hasAttribute("disabled")).toBe(true);
    fireEvent.click(resume);
    expect(command).toHaveBeenCalledOnce();
    expect(test.continuity).toHaveBeenCalledTimes(probeCount);
    await act(async () => { pending.reject({ code: "command_delivery_uncertain" }); await pending.promise.catch(() => undefined); });
    expect(test.attachRun).not.toHaveBeenCalled();
    expect(screen.queryByText(answer)).toBeNull();
    expect(test.startRun).not.toHaveBeenCalled();
    expect(editorText(await composer())).toBe(draft);
  });

  it.each(["accepted", "uncertain"] as const)("ignores an old conversation's %s Resume result after selecting another conversation", async (outcome) => {
    const user = userEvent.setup();
    const test = harness("live", false);
    const pending = deferred<Awaited<ReturnType<DesktopBridge["commandConversationQueue"]>>>();
    const command = vi.fn<DesktopBridge["commandConversationQueue"]>(() => pending.promise);
    const bridge = { ...test.bridge, commandConversationQueue: command };
    const view = render(panel(bridge));
    setEditorText(await composer(), draft);
    await settledLifecycle("idle");
    await user.click(await screen.findByRole("button", { name: "Open follow-up queue, 1 messages" }));
    await user.click(screen.getByRole("button", { name: "Resume queue" }));
    const other = { id: "http-queue-other", label: "Other conversation", runCount: 0 };
    view.rerender(panel(bridge, other));
    setEditorText(await composer(), "Other draft");
    await settledLifecycle("idle");
    const probeCount = test.continuity.mock.calls.length;
    await act(async () => {
      if (outcome === "uncertain") pending.reject({ code: "command_delivery_uncertain" });
      else pending.resolve({ commandId: "old-resume", clientId: "test", sessionId: session.id,
        action: "resume", expectedGeneration: "0", generation: "1", queue: { ...test.queue(session.id), paused: false }, replayed: false });
      await pending.promise.catch(() => undefined);
    });
    expect(test.continuity).toHaveBeenCalledTimes(probeCount);
    expect(test.attachRun).not.toHaveBeenCalled();
    expect(editorText(await composer())).toBe("Other draft");
    expect(screen.queryByText(answer)).toBeNull();
    expect(screen.queryByText("The queue changed or this action could not be recorded. Review the latest queue state.")).toBeNull();
  });

  it("does not accept a Resume receipt carrying another conversation's queue", async () => {
    const user = userEvent.setup();
    const test = harness("live", false);
    const command = vi.fn<DesktopBridge["commandConversationQueue"]>(async () => ({
      commandId: "wrong-scope", clientId: "test", sessionId: session.id, action: "resume",
      expectedGeneration: "0", generation: "1", replayed: false,
      queue: { ...test.queue("http-other-conversation"), paused: false },
    }));
    render(panel({ ...test.bridge, commandConversationQueue: command }));
    setEditorText(await composer(), draft);
    await settledLifecycle("idle");
    const probeCount = test.continuity.mock.calls.length;
    await user.click(await screen.findByRole("button", { name: "Open follow-up queue, 1 messages" }));
    await user.click(screen.getByRole("button", { name: "Resume queue" }));
    expect(await screen.findByText("The queue changed or this action could not be recorded. Review the latest queue state.")).toBeTruthy();
    expect(test.continuity).toHaveBeenCalledTimes(probeCount);
    expect(test.attachRun).not.toHaveBeenCalled();
    expect(test.startRun).not.toHaveBeenCalled();
    expect(editorText(await composer())).toBe(draft);
  });
});
