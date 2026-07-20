import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { App } from "./App";
import type { AppearanceSnapshot } from "./appearance/contract";
import { mergeTimelineEvent, reduceTimeline } from "./ConversationPanel";
import { DiffViewer } from "./DiffViewer";
import { Message } from "./Message";
import { MessageContent } from "./MessageContent";
import { presentTool, ToolCard } from "./ToolCard";
import type { DesktopBridge } from "./bridge";
import type {
  CatalogPage,
  DesktopBootstrap,
  RunStreamStatus,
  TimelineEvent,
  WorkspaceSummary,
} from "./types";

const originalMatchMedia = Object.getOwnPropertyDescriptor(window, "matchMedia");

afterEach(() => {
  cleanup();
  window.localStorage.clear();
  if (originalMatchMedia === undefined) delete (window as { matchMedia?: typeof window.matchMedia }).matchMedia;
  else Object.defineProperty(window, "matchMedia", originalMatchMedia);
});

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

const defaultAppearance: AppearanceSnapshot = {
  preference: "system",
  resolvedTheme: "dark",
};

type BridgeOverrides = Omit<Partial<DesktopBridge>, "bootstrap"> & {
  bootstrap?: () => Promise<Omit<DesktopBootstrap, "appearance"> & {
    appearance?: AppearanceSnapshot;
  }>;
};

function bridgeWith(overrides: BridgeOverrides = {}): DesktopBridge {
  const { bootstrap, ...remainingOverrides } = overrides;
  return {
    bootstrap: async () => ({
      appearance: defaultAppearance,
      ...(bootstrap === undefined
        ? { protocolVersion: 1, workspaces: [], recentWorkspaces: [] }
        : await bootstrap()),
    }),
    setAppearance: async (preference) => ({
      preference,
      resolvedTheme: preference === "light" ? "light" : "dark",
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
    renameSession: async (_workspaceId, input) => ({
      sessionRef: input.sessionRef,
      sessionId: input.sessionId,
      projectionGeneration: 2,
    }),
    deleteSession: async (_workspaceId, input) => ({
      sessionRef: input.sessionRef,
      sessionId: input.sessionId,
      projectionGeneration: 2,
    }),
    transcript: async () => ({
      totalMessages: 0,
      messages: [],
    }),
    runContext: async () => ({
      providerName: "deepseek",
      modelName: "deepseek-v4-flash",
      modelSelection: "fixed_for_session",
      defaultApprovalMode: "ask",
      availableApprovalModes: ["ask", "allow_readonly", "deny"],
      contextWindowTokens: 128_000,
      lastPromptTokens: 4_096,
      contextWindowSource: "provider",
    }),
    startRun: async (_workspaceId, sessionId) => ({
      id: "run-1",
      sessionId,
      status: "running",
      approvalMode: "ask",
      streamSequence: 0,
    }),
    attachRun: async (_workspaceId, sessionId, runId) => ({
      run: {
        id: runId,
        sessionId,
        status: "running",
        approvalMode: "ask",
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
      approvalMode: "ask",
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
    subscribeAppearance: async () => () => undefined,
    ...remainingOverrides,
  };
}

function installMediaQueries(matches: (query: string) => boolean): () => void {
  const original = Object.getOwnPropertyDescriptor(window, "matchMedia");
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    value: (query: string): MediaQueryList => ({
      matches: matches(query),
      media: query,
      onchange: null,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
      addListener: () => undefined,
      removeListener: () => undefined,
      dispatchEvent: () => true,
    }),
  });
  return () => {
    if (original === undefined) delete (window as { matchMedia?: typeof window.matchMedia }).matchMedia;
    else Object.defineProperty(window, "matchMedia", original);
  };
}

describe("desktop coding-agent components", () => {
  it("renders bounded markdown structure as text without raw HTML or navigation", async () => {
    const user = userEvent.setup();
    const originalClipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    render(
      <MessageContent text={"# Result\n\n<script>alert(1)</script>\n\n- first `item`\n- **second**\n\n_Ready to merge._\n\n```rust\ncargo test\n```"} />,
    );

    expect(document.querySelector("script")).toBeNull();
    expect(screen.getByText("<script>alert(1)</script>")).toBeTruthy();
    expect(screen.getByRole("list")).toBeTruthy();
    expect(screen.getByText("item").tagName).toBe("CODE");
    expect(screen.getByRole("heading", { name: "Result" }).tagName).toBe("H3");
    expect(screen.getByText("second").tagName).toBe("STRONG");
    expect(screen.getByText("Ready to merge.").tagName).toBe("EM");
    expect(screen.getByText("cargo test").tagName).toBe("CODE");
    expect(screen.queryByRole("link")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Copy code" }));
    expect(writeText).toHaveBeenCalledWith("cargo test");

    if (originalClipboard === undefined) delete (navigator as { clipboard?: Clipboard }).clipboard;
    else Object.defineProperty(navigator, "clipboard", originalClipboard);
  });

  it("keeps reasoning collapsed and renders read-only bounded tool and diff surfaces", () => {
    const { unmount } = render(
      <Message message={{ key: "reasoning", kind: "reasoning", label: "Working", text: "private scratch", status: "details" }} />,
    );
    expect((screen.getByText("Working").closest("details") as HTMLDetailsElement).open).toBe(false);
    unmount();

    const diff = "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-old\n+new";
    const diffRender = render(<DiffViewer diff={diff} />);
    expect(screen.getByLabelText("Unified diff")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /apply|revert/i })).toBeNull();
    diffRender.unmount();

    const output = Array.from({ length: 245 }, (_, index) => `line ${index + 1}`).join("\n");
    render(<ToolCard tool={{ key: "tool", toolName: "shell", text: output, status: "succeeded" }} />);
    expect(screen.getByText("5 output lines omitted from this view.")).toBeTruthy();
    expect(screen.queryByText("duration not recorded")).toBeNull();
    expect(screen.queryByText("risk not classified")).toBeNull();
  });

  it("summarizes structured tool failures and keeps transport JSON collapsed", () => {
    const text = JSON.stringify({
      content: "Tool execution was denied in Sigil Desktop.",
      error: { kind: "approval_denied", message: "Denied in Sigil Desktop", retriable: false },
      status: "error",
    });
    const tool = { key: "denied", toolName: "write_file", text };
    expect(presentTool(tool)).toMatchObject({
      displayName: "Write file",
      status: "Error",
      tone: "danger",
      summary: "Denied in Sigil Desktop",
      detailKind: "raw",
    });

    render(<ToolCard tool={tool} />);
    expect(screen.getByText("Denied in Sigil Desktop").tagName).toBe("P");
    const details = screen.getByText("Raw details").closest("details") as HTMLDetailsElement;
    expect(details.open).toBe(false);
    expect(screen.getByLabelText("write_file raw details").tagName).toBe("PRE");
  });
});

describe("desktop workspace and history shell", () => {
  it("persists a theme switch without remounting the active conversation draft", async () => {
    const user = userEvent.setup();
    const setAppearance = vi.fn(async () => ({
      preference: "light" as const,
      resolvedTheme: "light" as const,
    }));
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      setAppearance,
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    const composer = await screen.findByLabelText("Message Sigil") as HTMLTextAreaElement;
    await user.type(composer, "Keep this draft");
    await user.click(screen.getByRole("button", { name: "System theme. Switch to light theme" }));

    await waitFor(() => expect(document.documentElement.dataset.theme).toBe("light"));
    expect(setAppearance).toHaveBeenCalledWith("light");
    expect(screen.getByLabelText("Message Sigil")).toBe(composer);
    expect(composer.value).toBe("Keep this draft");
  });

  it("keeps the proven theme on save failure and exposes a scoped retry", async () => {
    const user = userEvent.setup();
    let attempts = 0;
    const bridge = bridgeWith({
      setAppearance: async () => {
        attempts += 1;
        if (attempts === 1) throw new Error("store unavailable");
        return { preference: "light", resolvedTheme: "light" };
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByRole("heading", { name: "Open a workspace" });
    await user.click(screen.getByRole("button", { name: "System theme. Switch to light theme" }));
    expect((await screen.findByRole("alert")).textContent).toContain("previous appearance is still active");
    expect(document.documentElement.dataset.theme).toBe("dark");

    await user.click(screen.getByRole("button", { name: "Theme change failed. Retry" }));
    await waitFor(() => expect(document.documentElement.dataset.theme).toBe("light"));
    expect(attempts).toBe(2);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("shows the current theme state and follows native updates", async () => {
    let listener: ((snapshot: AppearanceSnapshot) => void) | undefined;
    const bridge = bridgeWith({
      subscribeAppearance: async (next) => {
        listener = next;
        return () => undefined;
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByRole("heading", { name: "Open a workspace" });
    const systemTheme = screen.getByRole("button", { name: "System theme. Switch to light theme" });
    expect(systemTheme).toBeTruthy();
    expect(systemTheme.querySelector("circle")).not.toBeNull();
    act(() => listener?.({ preference: "system", resolvedTheme: "light" }));
    expect(document.documentElement.dataset.themePreference).toBe("system");
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(screen.getByRole("button", { name: "System theme. Switch to light theme" })).toBeTruthy();
  });

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

  it("groups one tool lifecycle into a single semantic timeline row", () => {
    const base = {
      workspaceId: workspace.id,
      sessionId: "session-1",
      runId: "run-tool",
      replayable: true,
      itemId: "call-1",
      toolName: "shell",
    };
    const rows = reduceTimeline([
      { ...base, sequence: 1, kind: "tool_started", status: "running" },
      { ...base, sequence: 2, kind: "tool_progress", text: "Running cargo test", status: "running" },
      { ...base, sequence: 3, kind: "tool_result", status: "succeeded" },
    ]);

    expect(rows).toEqual([expect.objectContaining({
      kind: "tool",
      label: "shell",
      text: "Running cargo test",
      status: "succeeded",
    })]);
  });

  it("restores the most recent workspace after native bootstrap", async () => {
    const openRecentWorkspace = vi.fn(async () => workspace);
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [],
        recentWorkspaces: [
          { id: workspace.id, displayName: "sigil", isOpen: false },
        ],
      }),
      openRecentWorkspace,
    });
    render(<App bridge={bridge} />);

    expect(await screen.findByRole("heading", { name: "Select a conversation" })).toBeTruthy();
    expect(openRecentWorkspace).toHaveBeenCalledWith(workspace.id);
    expect(screen.getByRole("complementary", { name: "Conversation navigation" })).toBeTruthy();
  });

  it("surfaces the actionable native error when a recent workspace cannot reopen", async () => {
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [],
        recentWorkspaces: [
          { id: workspace.id, displayName: "sigil", isOpen: false },
        ],
      }),
      openRecentWorkspace: async () => Promise.reject({
        code: "workspace_server_incompatible",
        message: "The desktop runtime is out of sync. Restart the development app or rebuild the package.",
      }),
    });
    render(<App bridge={bridge} />);

    await screen.findByRole("heading", { name: "Open a workspace" });
    expect((await screen.findByRole("alert")).textContent).toContain(
      "The desktop runtime is out of sync. Restart the development app or rebuild the package.",
    );
  });

  it("resizes and persists the desktop conversation sidebar", async () => {
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 1, workspaces: [workspace], recentWorkspaces: [] }),
    })} />);

    const separator = await screen.findByRole("separator", { name: "Resize conversation sidebar" });
    fireEvent.keyDown(separator, { key: "ArrowRight" });
    expect(separator.getAttribute("aria-valuenow")).toBe("336");
    expect(window.localStorage.getItem("sigil.desktop.navigation-width.v1")).toBe("336");
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

    await screen.findByRole("heading", { name: "Open a workspace" });
    await user.click(screen.getByRole("button", { name: "Open workspace" }));
    expect(await screen.findByText("Conversations")).toBeTruthy();
    expect(await screen.findByText("No matching conversation.")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "New conversation" }));
    expect(await screen.findByText("New conversation ready.")).toBeTruthy();
    expect(screen.getByText("0 recorded runs")).toBeTruthy();
    expect(screen.getAllByRole("heading", { name: "New conversation" })).toHaveLength(1);
    expect(screen.getByRole("complementary", { name: "Conversation navigation" })).toBeTruthy();
    expect(screen.getByRole("region", { name: "Conversation workspace" })).toBeTruthy();
    expect(screen.queryByRole("complementary", { name: "Verification" })).toBeNull();
    expect(screen.queryByRole("button", { name: /Open verification:/ })).toBeNull();
    expect(document.querySelector(".conversation-layout-with-review")).toBeNull();
    expect(screen.queryByText(/private bearer|TUI-first|stay in Rust/i)).toBeNull();

    await user.click(screen.getByRole("button", { name: "Switch workspace: sigil" }));
    await user.click(screen.getByRole("button", { name: "Close sigil" }));
    expect(await screen.findByText("Workspace closed.")).toBeTruthy();
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
    expect(screen.getAllByText("Unavailable")).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: /^First session/ }));
    await waitFor(() => expect(screen.getByText("2 recorded runs")).toBeTruthy());
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
    await user.click(screen.getByRole("button", { name: /^History with messages/ }));
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
        expiresAtMs: 4_102_444_800_000,
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
            approvalMode: "ask",
            streamSequence: 2,
          },
          events: [activeEvent, approvalEvent],
          streamState: "live",
          hasGap: true,
        };
      },
      cancelRun: async (_workspaceId, sessionId, runId) => {
        cancelledRun = runId;
        return { id: runId, sessionId, status: "cancel_requested", approvalMode: "ask", streamSequence: 3 };
      },
    });
    render(<App bridge={bridge} />);

    expect(await screen.findByText("Active session")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: /^Active session/ }));
    expect(await screen.findByText("Resume this work")).toBeTruthy();
    expect(screen.getByText(/Some live details were not retained/)).toBeTruthy();
    expect(screen.getByText("Review the resumed edit")).toBeTruthy();
    expect(order).toEqual(["events", "status", "attach"]);

    act(() => eventListener?.(activeEvent));
    expect(screen.getAllByText("Resume this work")).toHaveLength(1);
    await user.click(screen.getByRole("button", { name: "Stop run" }));
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
    await user.click(screen.getByRole("button", { name: /^Keep this conversation/ }));
    expect(await screen.findByRole("heading", { name: "Durable session" })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Filters" }));
    await user.type(screen.getByRole("textbox", { name: "Provider" }), "deepseek");
    expect(screen.getByText("1 active filter")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Durable session" })).toBeTruthy();
  });

  it("renames and explicitly confirms deletion from the conversation action menu", async () => {
    const renameSession = vi.fn(async (_workspaceId, input: { sessionRef: string; sessionId: string; displayName: string }) => ({
      sessionRef: input.sessionRef,
      sessionId: input.sessionId,
      projectionGeneration: 2,
    }));
    const deleteSession = vi.fn(async (_workspaceId, input: { sessionRef: string; sessionId: string }) => ({
      sessionRef: input.sessionRef,
      sessionId: input.sessionId,
      projectionGeneration: 3,
    }));
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "managed.jsonl",
          sessionId: "durable-managed",
          sourceState: "ready",
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Managed conversation",
          userMessageCount: 1,
          assistantMessageCount: 1,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      renameSession,
      deleteSession,
    });
    const user = userEvent.setup();
    render(<App bridge={bridge} />);

    await screen.findByText("Managed conversation");
    await user.click(screen.getByRole("button", { name: "Manage Managed conversation" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    const name = screen.getByRole("textbox", { name: "Conversation name" });
    await user.clear(name);
    await user.type(name, "Readable name");
    await user.click(screen.getByRole("button", { name: "Rename" }));
    await waitFor(() => expect(renameSession).toHaveBeenCalledWith(workspace.id, {
      sessionRef: "managed.jsonl",
      sessionId: "durable-managed",
      displayName: "Readable name",
    }));

    await user.click(screen.getByRole("button", { name: "Manage Managed conversation" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete" }));
    expect(deleteSession).not.toHaveBeenCalled();
    expect(screen.getByRole("alertdialog", { name: "Delete conversation?" })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Delete permanently" }));
    await waitFor(() => expect(deleteSession).toHaveBeenCalledWith(workspace.id, {
      sessionRef: "managed.jsonl",
      sessionId: "durable-managed",
    }));
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

    await screen.findByText("Conversations");
    await user.click(screen.getByRole("button", { name: "Switch workspace: sigil" }));
    const closeButton = screen.getByRole("button", { name: "Close sigil" });
    await user.click(closeButton);
    expect(await screen.findByRole("alertdialog")).toBeTruthy();
    expect(screen.getByText(/side effects that already happened are not undone/)).toBeTruthy();
    const keepRunning = screen.getByRole("button", { name: "Keep running" });
    const interruptRuns = screen.getByRole("button", { name: "Close workspace and interrupt runs" });
    expect(document.activeElement).toBe(keepRunning);
    fireEvent.keyDown(document, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(interruptRuns);
    fireEvent.keyDown(document, { key: "Tab" });
    expect(document.activeElement).toBe(keepRunning);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("alertdialog")).toBeNull();
    const workspaceTrigger = screen.getByRole("button", { name: "Switch workspace: sigil" });
    expect(document.activeElement).toBe(workspaceTrigger);

    await user.click(workspaceTrigger);
    await user.click(screen.getByRole("button", { name: "Close sigil" }));
    await user.click(screen.getByRole("button", { name: "Close workspace and interrupt runs" }));
    expect(await screen.findByText("Workspace closed.")).toBeTruthy();
    expect(confirmations).toEqual([false, false, true]);
  });

  it("uses primitive navigation and contextual review drawers without remounting the conversation", async () => {
    const restoreMedia = installMediaQueries((query) => query.includes("max-width"));
    const user = userEvent.setup();
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      verification: async () => ({
        taskId: "task-compact",
        stepId: "verify-compact",
        scopeKind: "task",
        scopeId: "task-compact",
        verdict: "passed",
        status: "passed",
        evidence: {},
      }),
    });
    render(<App bridge={bridge} />);

    await screen.findByRole("button", { name: "Browse conversations" });
    expect(document.querySelector("#desktop-navigation")).toBeNull();
    const navigationTrigger = screen.getByRole("button", { name: "Browse conversations" });
    await user.click(navigationTrigger);
    const navigation = screen.getByRole("dialog", { name: "Browse conversations" });
    expect(navigation.id).toBe("desktop-navigation");
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Close Browse conversations" }));
    expect(await screen.findByText("No matching conversation.")).toBeTruthy();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Browse conversations" })).toBeNull();
    expect(document.activeElement).toBe(navigationTrigger);

    await user.click(screen.getByRole("button", { name: "New conversation" }));
    expect(document.querySelector("#verification-inspector")).toBeNull();
    const timeline = screen.getByRole("log", { name: "Conversation timeline" });
    const composer = screen.getByLabelText("Message Sigil") as HTMLTextAreaElement;
    timeline.scrollTop = 72;
    await user.type(composer, "Preserve this draft");
    const reviewTrigger = await screen.findByRole("button", { name: "Open verification: passed" });
    await user.click(reviewTrigger);
    const inspector = screen.getByRole("dialog", { name: "Verification" });
    expect(inspector.id).toBe("verification-inspector");
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Close Verification" }));
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Verification" })).toBeNull();
    expect(document.activeElement).toBe(reviewTrigger);
    expect(screen.getByRole("log", { name: "Conversation timeline" })).toBe(timeline);
    expect(timeline.scrollTop).toBe(72);
    expect(screen.getByLabelText("Message Sigil")).toBe(composer);
    expect(composer.value).toBe("Preserve this draft");
    expect(timeline.getAttribute("aria-live")).toBe("off");

    cleanup();
    restoreMedia();
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
    expect(screen.getByRole("button", { name: "Refresh conversations" })).toBeTruthy();
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
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(await screen.findByText("Run started. Live updates are connected.")).toBeTruthy();
    expect(document.querySelector(".statusbar")).toBeNull();
    expect(document.querySelector(".app-shell > .sr-only")?.textContent).toContain("Sigil is ready.");
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
    expect(screen.getByText("Run finished. Review the final response and verification status.")).toBeTruthy();
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
        return { id: "run-ime", sessionId, status: "running", approvalMode: "ask", streamSequence: 0 };
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
    await user.click(screen.getByRole("button", { name: "Send message" }));

    expect(prompts).toEqual(["请检查 中文输入，包含粘贴"]);
  });

  it("projects model and context facts and sends the selected approval mode", async () => {
    const user = userEvent.setup();
    let selectedMode = "";
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      startRun: async (_workspaceId, sessionId, _prompt, approvalMode) => {
        selectedMode = approvalMode;
        return {
          id: "run-mode",
          sessionId,
          status: "running",
          approvalMode,
          streamSequence: 0,
        };
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    expect(await screen.findByRole("button", { name: "deepseek-v4-flash" })).toBeTruthy();
    expect(screen.getByRole("meter", { name: "Context usage 3%" })).toBeTruthy();
    await user.selectOptions(screen.getByRole("combobox", { name: "Approval mode" }), "allow_readonly");
    const composer = screen.getByLabelText("Message Sigil") as HTMLTextAreaElement;
    Object.defineProperty(composer, "scrollHeight", { configurable: true, value: 240 });
    await user.type(composer, "Inspect safely");
    expect(composer.style.height).toBe("176px");
    expect(composer.style.overflowY).toBe("auto");
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(selectedMode).toBe("allow_readonly");
  });

  it("sends with Enter, keeps active-run input editable, and restores a session draft", async () => {
    const user = userEvent.setup();
    const prompts: string[] = [];
    const draftValues = new Map<string, string>();
    const originalStorage = Object.getOwnPropertyDescriptor(window, "localStorage");
    Object.defineProperty(window, "localStorage", { configurable: true, value: {
      get length() { return draftValues.size; },
      clear: () => draftValues.clear(),
      getItem: (key: string) => draftValues.get(key) ?? null,
      key: (index: number) => [...draftValues.keys()][index] ?? null,
      removeItem: (key: string) => { draftValues.delete(key); },
      setItem: (key: string, value: string) => { draftValues.set(key, value); },
    } satisfies Storage });
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 1,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      startRun: async (_workspaceId, sessionId, prompt) => {
        prompts.push(prompt);
        return { id: "run-draft", sessionId, status: "running", approvalMode: "ask", streamSequence: 0 };
      },
    });
    const first = render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    const composer = screen.getByLabelText("Message Sigil") as HTMLTextAreaElement;
    await user.type(composer, "Run this after Shift+Enter");
    fireEvent.keyDown(composer, { key: "Enter", shiftKey: true });
    expect(prompts).toEqual([]);
    fireEvent.keyDown(composer, { key: "Enter" });
    await waitFor(() => expect(prompts).toEqual(["Run this after Shift+Enter"]));
    expect(composer.disabled).toBe(false);
    await user.type(composer, "Follow-up draft");
    expect(draftValues.get(`sigil:conversation-draft:v1:${workspace.id}:http-session-new`)).toBe("Follow-up draft");

    first.unmount();
    render(<App bridge={bridge} />);
    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    expect((screen.getByLabelText("Message Sigil") as HTMLTextAreaElement).value).toBe("Follow-up draft");
    if (originalStorage !== undefined) Object.defineProperty(window, "localStorage", originalStorage);
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
        return { id: runId, sessionId, status: "cancel_requested", approvalMode: "ask", streamSequence: 4 };
      },
      verification: async () => ({
        taskId: "task-approval",
        stepId: "verify-approval",
        scopeKind: "task",
        scopeId: "task-approval",
        verdict: "passed",
        status: "passed",
        evidence: {},
      }),
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    await user.type(screen.getByLabelText("Message Sigil"), "Edit a file");
    await user.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() => expect(eventListener).toBeDefined());
    await user.click(await screen.findByRole("button", { name: "Open verification: passed" }));
    expect(screen.getByRole("dialog", { name: "Verification" })).toBeTruthy();
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
          expiresAtMs: 4_102_444_800_000,
          operation: "edit_file",
          risk: "medium",
          snapshotRequired: true,
          previewTitle: "Edit one file",
          previewSummary: "Review the proposed edit",
          previewBody: "- old\n+ new",
        },
      });
    });

    const approvalTitle = await screen.findByText("Edit one file");
    const approvalDock = approvalTitle.closest("section") as HTMLElement;
    expect(screen.queryByRole("dialog", { name: "Verification" })).toBeNull();
    expect(screen.getByRole("button", { name: "Open verification: passed" }).hasAttribute("disabled")).toBe(true);
    expect(document.activeElement).toBe(approvalDock);
    fireEvent.keyDown(approvalDock, { key: "Escape" });
    expect(document.activeElement).toBe(screen.getByLabelText("Message Sigil"));
    await user.click(screen.getByRole("button", { name: "Approve once" }));
    expect(approvedCall).toBe("run-1:approval-1:true");
    act(() => {
      eventListener?.({
        workspaceId: workspace.id,
        sessionId: "http-session-new",
        runId: "run-1",
        sequence: 4,
        replayable: true,
        kind: "approval_resolved",
        itemId: "call-1",
        status: "approved",
      });
    });
    expect(screen.queryByText("Edit one file")).toBeNull();
    expect(document.activeElement).toBe(screen.getByLabelText("Message Sigil"));
    await user.click(screen.getByRole("button", { name: "Stop run" }));
    expect(cancelledRun).toBe("run-1");
    expect(await screen.findByText("Cancellation requested. Waiting for the run to stop safely.")).toBeTruthy();
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
    const reviewTrigger = await screen.findByRole("button", { name: "Open verification: check failed" });
    expect(screen.queryByRole("dialog", { name: "Verification" })).toBeNull();
    await user.click(reviewTrigger);
    expect(await screen.findByRole("dialog", { name: "Verification" })).toBeTruthy();
    expect(screen.getByText("2 tests failed")).toBeTruthy();
    expect((screen.getByText("Evidence details").closest("details") as HTMLDetailsElement).open).toBe(false);
    expect(screen.getByText("receipt-1")).toBeTruthy();
    expect(screen.getByText("changeset-1")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Run recommended check" }));
    expect(await screen.findByText("passed")).toBeTruthy();
    expect(rerunSnapshot).toBe("snapshot-1");
  });
});
