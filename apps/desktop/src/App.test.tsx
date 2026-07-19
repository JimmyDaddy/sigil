import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { mergeTimelineEvent, reduceTimeline } from "./ConversationPanel";
import type { DesktopBridge } from "./bridge";
import type {
  CatalogPage,
  RunStreamStatus,
  TimelineEvent,
  WorkspaceSummary,
} from "./types";

afterEach(cleanup);

const workspace: WorkspaceSummary = {
  id: "workspace-0123456789ab",
  displayName: "sigil",
  serverVersion: "0.0.1-alpha.5",
  state: "ready",
};

const emptyCatalog: CatalogPage = {
  workspaceId: workspace.id,
  generation: 1,
  reconciledAtUnixMs: 1_784_419_200_000,
  degradedSourceCount: 0,
  identityConflictCount: 0,
  truncatedSourceCount: 0,
  entries: [],
};

function bridgeWith(overrides: Partial<DesktopBridge> = {}): DesktopBridge {
  return {
    bootstrap: async () => ({
      protocolVersion: 1,
      workspaces: [],
      recentWorkspaces: [],
    }),
    pickWorkspace: async () => ({ cancelled: false, workspace }),
    openRecentWorkspace: async () => workspace,
    closeWorkspace: async () => [],
    catalog: async () => emptyCatalog,
    createSession: async () => ({
      id: "http-session-new",
      label: "New conversation",
      runCount: 0,
    }),
    openSession: async () => ({
      id: "http-session-open",
      label: "Durable session",
      runCount: 2,
    }),
    transcript: async () => ({
      totalMessages: 0,
      messages: [],
    }),
    startRun: async (_workspaceId, sessionId) => ({
      id: "run-1",
      sessionId,
      status: "running",
      streamSequence: 0,
    }),
    attachRun: async (_workspaceId, sessionId, runId) => ({
      run: {
        id: runId,
        sessionId,
        status: "running",
        streamSequence: 0,
      },
      events: [],
      streamState: "live",
      hasGap: false,
    }),
    cancelRun: async (_workspaceId, sessionId, runId) => ({
      id: runId,
      sessionId,
      status: "cancel_requested",
      streamSequence: 1,
    }),
    resolveApproval: async (_workspaceId, _sessionId, runId, approval, approve) => ({
      runId,
      callId: approval.callId,
      decision: approve ? "approved" : "denied",
    }),
    verification: async () => {
      throw new Error("no verification projection");
    },
    rerunVerification: async () => {
      throw new Error("no verification projection");
    },
    subscribeRunEvents: async () => () => undefined,
    subscribeRunStreamStatus: async () => () => undefined,
    ...overrides,
  };
}

describe("desktop workspace and history shell", () => {
  it("keeps cross-run timeline order by arrival instead of opaque run id", () => {
    const base = {
      workspaceId: workspace.id,
      sessionId: "session-1",
      replayable: true,
    };
    const first: TimelineEvent = {
      ...base,
      runId: "run-z",
      sequence: 1,
      kind: "run_started",
      text: "First run",
    };
    const second: TimelineEvent = {
      ...base,
      runId: "run-a",
      sequence: 1,
      kind: "run_started",
      text: "Second run",
    };

    const rows = reduceTimeline(
      mergeTimelineEvent(mergeTimelineEvent([], first), second),
    );
    expect(rows.map((row) => row.text)).toEqual(["First run", "Second run"]);
  });

  it("renders the honest empty and recent-workspace states after native bootstrap", async () => {
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [],
        recentWorkspaces: [
          { id: workspace.id, displayName: "sigil", isOpen: false },
        ],
      }),
    });
    render(<App bridge={bridge} />);

    expect(await screen.findByText("No server is running.")).toBeTruthy();
    expect(screen.getByText("Reopen")).toBeTruthy();
    expect(screen.getByText("Choose a workspace to begin.")).toBeTruthy();
  });

  it("opens a workspace, creates a conversation, and closes the owned server", async () => {
    const user = userEvent.setup();
    let closeRequest = "";
    const bridge = bridgeWith({
      closeWorkspace: async (workspaceId) => {
        closeRequest = workspaceId;
        return [];
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No server is running.");
    await user.click(screen.getAllByRole("button", { name: "Choose workspace" })[0]);
    expect(await screen.findByText("Conversation history")).toBeTruthy();
    expect(await screen.findByText("No matching conversation.")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "New conversation" }));
    expect(await screen.findByText("Conversation ready")).toBeTruthy();
    expect(screen.getByText("0 existing runs")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Close sigil" }));
    expect(await screen.findByText("Workspace server closed.")).toBeTruthy();
    expect(closeRequest).toBe(workspace.id);
  });

  it("pages generation-consistent history and opens only a ready durable entry", async () => {
    const user = userEvent.setup();
    const cursors: Array<string | undefined> = [];
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [
          { id: workspace.id, displayName: "sigil", isOpen: true },
        ],
      }),
      catalog: async (_workspaceId, request) => {
        cursors.push(request.cursor);
        return request.cursor === undefined
          ? {
              ...emptyCatalog,
              entries: [
                {
                  sessionRef: "first.jsonl",
                  sessionId: "durable-first",
                  sourceState: "ready",
                  sourceModifiedAtUnixMs: 1_784_419_200_000,
                  providerName: "deepseek",
                  modelName: "deepseek-chat",
                  title: "First session",
                  userMessageCount: 3,
                  assistantMessageCount: 3,
                  toolResultCount: 1,
                  pinned: false,
                },
              ],
              nextCursor: "cursor-2",
            }
          : {
              ...emptyCatalog,
              entries: [
                {
                  sessionRef: "legacy.jsonl",
                  sourceState: "unsupported_legacy",
                  sourceModifiedAtUnixMs: 1_784_419_100_000,
                  title: "Legacy session",
                  userMessageCount: 0,
                  assistantMessageCount: 0,
                  toolResultCount: 0,
                  pinned: false,
                },
              ],
            };
      },
    });
    render(<App bridge={bridge} />);

    expect(await screen.findByText("First session")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Load more" }));
    expect(await screen.findByText("Legacy session")).toBeTruthy();
    expect(cursors).toEqual([undefined, "cursor-2"]);
    expect(
      (screen.getByRole("button", { name: "Inspect only" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);

    await user.click(screen.getByRole("button", { name: "Open" }));
    await waitFor(() => expect(screen.getByText("2 existing runs")).toBeTruthy());
  });

  it("opens bounded transcript text and pages older messages in chronological order", async () => {
    const user = userEvent.setup();
    const transcriptQueries: Array<number | undefined> = [];
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [
          {
            sessionRef: "history.jsonl",
            sessionId: "durable-history",
            sourceState: "ready",
            sourceModifiedAtUnixMs: 1_784_419_200_000,
            title: "History with messages",
            userMessageCount: 2,
            assistantMessageCount: 1,
            toolResultCount: 1,
            pinned: false,
          },
        ],
      }),
      transcript: async (_workspaceId, _sessionId, request) => {
        transcriptQueries.push(request.before);
        return request.before === undefined
          ? {
              totalMessages: 4,
              messages: [
                {
                  ordinal: 3,
                  messageId: "message-tool",
                  role: "tool",
                  content: "cargo test passed",
                  toolName: "shell",
                  imageAttachmentCount: 0,
                  truncated: false,
                  originalContentBytes: 17,
                },
                {
                  ordinal: 4,
                  messageId: "message-final",
                  role: "assistant",
                  content: "The change is complete.",
                  assistantKind: "final_answer",
                  imageAttachmentCount: 0,
                  truncated: false,
                  originalContentBytes: 23,
                },
              ],
              nextBefore: 3,
            }
          : {
              totalMessages: 4,
              messages: [
                {
                  ordinal: 1,
                  messageId: "message-user",
                  role: "user",
                  content: "Fix the parser",
                  imageAttachmentCount: 0,
                  truncated: false,
                  originalContentBytes: 14,
                },
                {
                  ordinal: 2,
                  messageId: "message-preamble",
                  role: "assistant",
                  content: "I will inspect it.",
                  assistantKind: "tool_preamble",
                  imageAttachmentCount: 0,
                  truncated: false,
                  originalContentBytes: 18,
                },
              ],
            };
      },
    });
    render(<App bridge={bridge} />);

    expect(await screen.findByText("History with messages")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Open" }));
    expect(await screen.findByText("cargo test passed")).toBeTruthy();
    expect(screen.getByText("The change is complete.")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: /Load earlier messages/ }));
    const older = await screen.findByText("Fix the parser");
    const latest = screen.getByText("The change is complete.");
    expect(older.compareDocumentPosition(latest) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
    expect(transcriptQueries).toEqual([undefined, 3]);
  });

  it("reattaches an active run after listeners and restores bounded controls with honest gaps", async () => {
    const user = userEvent.setup();
    const order: string[] = [];
    let eventListener: ((event: TimelineEvent) => void) | undefined;
    let cancelledRun = "";
    const activeEvent: TimelineEvent = {
      workspaceId: workspace.id,
      sessionId: "http-session-active",
      runId: "run-active",
      sequence: 1,
      replayable: true,
      kind: "run_started",
      text: "Resume this work",
    };
    const approvalEvent: TimelineEvent = {
      ...activeEvent,
      sequence: 2,
      kind: "approval_requested",
      itemId: "call-active",
      toolName: "write_file",
      approval: {
        callId: "call-active",
        toolName: "write_file",
        approvalRequestId: "approval-active",
        toolCallHash: "hash-active",
        policyVersion: "policy-active",
        expiresAtMs: 1_784_419_200_000,
        snapshotRequired: true,
        previewTitle: "Review the resumed edit",
      },
    };
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "active.jsonl",
          sessionId: "durable-active",
          sourceState: "ready",
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Active session",
          userMessageCount: 1,
          assistantMessageCount: 0,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({
        id: "http-session-active",
        label: "Active session",
        runCount: 1,
        foregroundRunId: "run-active",
      }),
      subscribeRunEvents: async (listener) => {
        order.push("events");
        eventListener = listener;
        return () => undefined;
      },
      subscribeRunStreamStatus: async () => {
        order.push("status");
        return () => undefined;
      },
      attachRun: async () => {
        order.push("attach");
        return {
          run: {
            id: "run-active",
            sessionId: "http-session-active",
            status: "waiting_for_approval",
            streamSequence: 2,
          },
          events: [activeEvent, approvalEvent],
          streamState: "live",
          hasGap: true,
        };
      },
      cancelRun: async (_workspaceId, sessionId, runId) => {
        cancelledRun = runId;
        return { id: runId, sessionId, status: "cancel_requested", streamSequence: 3 };
      },
    });
    render(<App bridge={bridge} />);

    expect(await screen.findByText("Active session")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Open" }));
    expect(await screen.findByText("Resume this work")).toBeTruthy();
    expect(screen.getByText(/Some live details were not retained/)).toBeTruthy();
    expect(screen.getByText("Review the resumed edit")).toBeTruthy();
    expect(order).toEqual(["events", "status", "attach"]);

    act(() => eventListener?.(activeEvent));
    expect(screen.getAllByText("Resume this work")).toHaveLength(1);
    await user.click(screen.getByRole("button", { name: "Cancel run" }));
    expect(cancelledRun).toBe("run-active");
  });

  it("keeps the opened conversation mounted while history filters refresh", async () => {
    const user = userEvent.setup();
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "keep.jsonl",
          sessionId: "durable-keep",
          sourceState: "ready",
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Keep this conversation",
          userMessageCount: 1,
          assistantMessageCount: 1,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
    });
    render(<App bridge={bridge} />);

    expect(await screen.findByText("Keep this conversation")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Open" }));
    expect(await screen.findByText("Conversation ready")).toBeTruthy();
    await user.type(screen.getByRole("textbox", { name: "Filter by provider" }), "deepseek");
    expect(screen.getByText("Conversation ready")).toBeTruthy();
  });

  it("requires explicit confirmation before closing a workspace with active runs", async () => {
    const user = userEvent.setup();
    const confirmations: boolean[] = [];
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      closeWorkspace: async (_workspaceId, confirmActiveRuns = false) => {
        confirmations.push(confirmActiveRuns);
        if (!confirmActiveRuns) {
          throw {
            code: "workspace_active_runs",
            message: "1 active run(s) still belong to this workspace.",
          };
        }
        return [];
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("Conversation history");
    await user.click(screen.getByRole("button", { name: "Close sigil" }));
    expect(await screen.findByRole("alertdialog")).toBeTruthy();
    expect(screen.getByText(/side effects that already happened are not undone/)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Close workspace and interrupt runs" }));
    expect(await screen.findByText("Workspace server closed.")).toBeTruthy();
    expect(confirmations).toEqual([false, true]);
  });

  it("shows a stale pagination recovery instead of mixing generations", async () => {
    const user = userEvent.setup();
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async (_workspaceId, request) => {
        if (request.cursor !== undefined) throw { code: "catalog_stale" };
        return { ...emptyCatalog, nextCursor: "stale-cursor" };
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "Load more" }));
    expect(await screen.findByText("History changed while paging.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Refresh history" })).toBeTruthy();
  });

  it("runs a prompt and merges streamed and durable completion into one assistant reply", async () => {
    const user = userEvent.setup();
    let eventListener: ((event: TimelineEvent) => void) | undefined;
    let statusListener: ((status: RunStreamStatus) => void) | undefined;
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      subscribeRunEvents: async (listener) => {
        eventListener = listener;
        return () => undefined;
      },
      subscribeRunStreamStatus: async (listener) => {
        statusListener = listener;
        return () => undefined;
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    await user.type(screen.getByLabelText("Message Sigil"), "Say hello");
    await user.click(screen.getByRole("button", { name: "Run" }));
    await waitFor(() => expect(eventListener).toBeDefined());

    const base = {
      workspaceId: workspace.id,
      sessionId: "http-session-new",
      runId: "run-1",
      replayable: false,
    };
    act(() => {
      eventListener?.({ ...base, sequence: 1, kind: "run_started", text: "Say hello" });
      eventListener?.({ ...base, sequence: 2, kind: "assistant_delta", text: "Hel" });
      eventListener?.({ ...base, sequence: 3, kind: "assistant_message", text: "Hello" });
      eventListener?.({ ...base, sequence: 4, kind: "run_finished", text: "Hello", replayable: true });
      statusListener?.({ ...base, state: "terminal" });
    });

    expect(screen.getAllByText("Hello")).toHaveLength(1);
    expect(screen.getByText("complete")).toBeTruthy();
    expect(screen.getByText("terminal")).toBeTruthy();
  });

  it("preserves IME text, accepts clipboard input, and does not submit during composition", async () => {
    const user = userEvent.setup();
    const prompts: string[] = [];
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      startRun: async (_workspaceId, sessionId, prompt) => {
        prompts.push(prompt);
        return { id: "run-ime", sessionId, status: "running", streamSequence: 0 };
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    const composer = screen.getByLabelText("Message Sigil") as HTMLTextAreaElement;
    composer.focus();
    fireEvent.compositionStart(composer);
    fireEvent.change(composer, { target: { value: "请检查 中文输入" } });
    fireEvent.keyDown(composer, { key: "Enter", code: "Enter", isComposing: true });
    expect(prompts).toEqual([]);
    fireEvent.compositionEnd(composer);
    await user.paste("，包含粘贴");
    await user.click(screen.getByRole("button", { name: "Run" }));

    expect(prompts).toEqual(["请检查 中文输入，包含粘贴"]);
  });

  it("follows new timeline rows only while the reader stays near the end", async () => {
    const user = userEvent.setup();
    let eventListener: ((event: TimelineEvent) => void) | undefined;
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      subscribeRunEvents: async (listener) => {
        eventListener = listener;
        return () => undefined;
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    await waitFor(() => expect(eventListener).toBeDefined());
    const timeline = screen.getByRole("log", { name: "Conversation timeline" });
    Object.defineProperties(timeline, {
      clientHeight: { configurable: true, value: 100 },
      scrollHeight: { configurable: true, value: 600 },
    });
    const base = {
      workspaceId: workspace.id,
      sessionId: "http-session-new",
      runId: "run-scroll",
      replayable: false,
    };
    act(() => {
      eventListener?.({ ...base, sequence: 1, kind: "run_started", text: "First" });
    });
    expect(timeline.scrollTop).toBe(600);

    timeline.scrollTop = 100;
    fireEvent.scroll(timeline);
    act(() => {
      eventListener?.({ ...base, sequence: 2, kind: "assistant_message", text: "Second" });
    });
    expect(timeline.scrollTop).toBe(100);
  });

  it("submits the exact approval guard and requests cooperative cancellation", async () => {
    const user = userEvent.setup();
    let eventListener: ((event: TimelineEvent) => void) | undefined;
    let approvedCall = "";
    let cancelledRun = "";
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      subscribeRunEvents: async (listener) => {
        eventListener = listener;
        return () => undefined;
      },
      resolveApproval: async (_workspaceId, _sessionId, runId, approval, approve) => {
        approvedCall = `${runId}:${approval.approvalRequestId}:${approve}`;
        return { runId, callId: approval.callId, decision: approve ? "approved" : "denied" };
      },
      cancelRun: async (_workspaceId, sessionId, runId) => {
        cancelledRun = runId;
        return { id: runId, sessionId, status: "cancel_requested", streamSequence: 4 };
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    await user.type(screen.getByLabelText("Message Sigil"), "Edit a file");
    await user.click(screen.getByRole("button", { name: "Run" }));
    await waitFor(() => expect(eventListener).toBeDefined());
    act(() => {
      eventListener?.({
        workspaceId: workspace.id,
        sessionId: "http-session-new",
        runId: "run-1",
        sequence: 3,
        replayable: true,
        kind: "approval_requested",
        itemId: "call-1",
        toolName: "write_file",
        approval: {
          callId: "call-1",
          toolName: "write_file",
          approvalRequestId: "approval-1",
          toolCallHash: "hash-1",
          policyVersion: "policy-1",
          expiresAtMs: 1_784_419_200_000,
          operation: "edit_file",
          risk: "medium",
          snapshotRequired: true,
          previewTitle: "Edit one file",
          previewSummary: "Review the proposed edit",
          previewBody: "- old\n+ new",
        },
      });
    });

    expect(await screen.findByText("Edit one file")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Approve once" }));
    expect(approvedCall).toBe("run-1:approval-1:true");
    await user.click(screen.getByRole("button", { name: "Cancel run" }));
    expect(cancelledRun).toBe("run-1");
    expect(await screen.findByText("Cancellation requested. Waiting for durable cleanup evidence.")).toBeTruthy();
  });

  it("shows exact verification evidence and reruns only the rendered binding", async () => {
    const user = userEvent.setup();
    let rerunSnapshot = "";
    const verification = {
      taskId: "task_1",
      stepId: "verify_1",
      scopeKind: "step" as const,
      scopeId: "task_1:verify_1",
      verdict: "failed" as const,
      status: "check failed",
      recommendedCheckSpecId: "cargo-test",
      recommendationReason: "the latest result failed for the current task scope",
      action: {
        kind: "rerun" as const,
        request: {
          taskId: "task_1",
          stepId: "verify_1",
          checkSpecId: "cargo-test",
          checkSpecHash: "check-hash",
          policyHash: "policy-hash",
          workspaceSnapshotId: "snapshot-1",
        },
      },
      evidence: {
        receiptId: "receipt-1",
        workspaceSnapshotId: "snapshot-1",
        changesetId: "changeset-1",
        failureSummary: "2 tests failed",
        commandEventId: "event-command",
        outputArtifactId: "artifact-output",
      },
    };
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      verification: async () => verification,
      rerunVerification: async (_workspaceId, _sessionId, request) => {
        rerunSnapshot = request.workspaceSnapshotId;
        return { ...verification, verdict: "passed", status: "passed", action: undefined };
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    expect(await screen.findByText("2 tests failed")).toBeTruthy();
    expect(screen.getByText("receipt-1")).toBeTruthy();
    expect(screen.getByText("changeset-1")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Run recommended check" }));
    expect(await screen.findByText("passed")).toBeTruthy();
    expect(rerunSnapshot).toBe("snapshot-1");
  });
});
