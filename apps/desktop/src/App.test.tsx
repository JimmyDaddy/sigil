import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { App } from "./App";
import type { AppearanceSnapshot } from "./appearance/contract";
import { mergeTimelineEvent, reduceConversationTimeline, reduceTimeline } from "./ConversationPanel";
import { DiffViewer } from "./DiffViewer";
import { ErrorCard } from "./ErrorCard";
import { Message } from "./Message";
import { MessageContent } from "./MessageContent";
import { presentTool, ToolCard } from "./ToolCard";
import type { DesktopBridge } from "./bridge";
import type {
  CatalogPage,
  DesktopBootstrap,
  RunContext,
  RunStreamStatus,
  SessionSummary,
  TimelineEvent,
  TranscriptMessage,
  WorkspaceSummary,
} from "./types";
import { Icon } from "./ui/icons";

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

const defaultRunContext: RunContext = {
  providerName: "deepseek",
  modelName: "deepseek-v4-flash",
  availableModels: ["deepseek-v4-flash", "deepseek-v4-pro"],
  modelOptions: [
    {
      modelName: "deepseek-v4-flash",
      availableReasoningEfforts: ["low", "medium", "high", "max"],
      defaultReasoningEffort: "max",
      reasoningEffortBinding: "effort-binding-deepseek-v4-flash",
    },
    {
      modelName: "deepseek-v4-pro",
      availableReasoningEfforts: ["low", "medium", "high", "max"],
      defaultReasoningEffort: "max",
      reasoningEffortBinding: "effort-binding-deepseek-v4-pro",
    },
  ],
  modelSelection: "per_run",
  modelSelectionBinding: "model-binding-deepseek-v4-flash",
  defaultPermissionMode: "manual",
  availablePermissionModes: ["read-only", "manual", "auto-edit", "danger-full-access"],
  availableReasoningEfforts: ["low", "medium", "high", "max"],
  defaultReasoningEffort: "max",
  reasoningEffortBinding: "effort-binding-deepseek-v4-flash",
  contextWindowTokens: 128_000,
  lastPromptTokens: 4_096,
  contextWindowSource: "provider",
  extensionCatalog: { commands: [], skills: [], agents: [] },
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
        ? { protocolVersion: 2, workspaces: [], recentWorkspaces: [] }
        : await bootstrap()),
    }),
    setAppearance: async (preference) => ({
      preference,
      resolvedTheme: preference === "light" ? "light" : "dark",
    }),
    openExternalUrl: async () => undefined,
    supportDoctor: async () => ({
      generatedAtUnixMs: 1_784_419_200_000,
      version: "0.0.1-alpha.5",
      commit: "test",
      target: "aarch64-apple-darwin",
      profile: "debug",
      environment: { os: "macos", architecture: "aarch64", terminalFamily: "other" },
      summary: { overallStatus: "warn", ok: 4, warn: 1, error: 0 },
      checks: [{ status: "warn", name: "configuration", summary: "Review one setting.", remediation: "Open sigil.toml." }],
      privacy: { included: ["build metadata"], excluded: ["credentials"], reviewBeforeSharing: true },
    }),
    exportSupportBundle: async () => ({ cancelled: false, fileName: "sigil-support-test.json" }),
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
    quarantineSession: async (_workspaceId, input) => ({
      sessionRef: input.sessionRef,
      quarantineName: `quarantined--${input.sessionRef}`,
      projectionGeneration: 2,
    }),
    deleteInvalidSessionSource: async (_workspaceId, input) => ({
      sessionRef: input.sessionRef,
      projectionGeneration: 2,
    }),
    planSessionCatalogBatch: async (_workspaceId, input) => ({
      planId: "sha256:test-plan",
      action: input.action,
      generation: 1,
      total: input.items.length,
      executable: input.items.length,
      blocked: 0,
      items: input.items.map((item) => ({ sessionRef: item.sessionRef, status: "executable" })),
    }),
    executeSessionCatalogBatch: async (_workspaceId, input) => ({
      planId: input.planId,
      action: input.action,
      total: input.items.length,
      completed: input.items.length,
      failed: 0,
      skipped: 0,
      items: input.items.map((item) => ({ sessionRef: item.sessionRef, outcome: "completed" })),
    }),
    transcript: async () => ({
      totalMessages: 0,
      messages: [],
    }),
    runContext: async () => defaultRunContext,
    agentActivity: async () => ({
      totalAgents: 0,
      activeAgents: 0,
      terminalAgents: 0,
      items: [],
    }),
    startRun: async (_workspaceId, sessionId) => ({
      id: "run-1",
      sessionId,
      status: "running",
      permissionMode: "manual",
      streamSequence: 0,
    }),
    attachRun: async (_workspaceId, sessionId, runId) => ({
      run: {
        id: runId,
        sessionId,
        status: "running",
        permissionMode: "manual",
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
      permissionMode: "manual",
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
  it("renders safe GFM and local syntax highlighting without raw HTML or renderer navigation", async () => {
    const user = userEvent.setup();
    const originalClipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");
    const writeText = vi.fn(async () => undefined);
    const openExternalUrl = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    render(
      <MessageContent
        onOpenExternalUrl={openExternalUrl}
        text={"# Result\n\n<script>alert(1)</script>\n\n![tracking](https://example.com/pixel.png)\n\n- first `item`\n- **second**\n- [x] verified\n\n~~stale~~ and _ready_.\n\n| Check | State |\n| --- | --- |\n| tests | pass |\n\n[Docs](https://example.com/docs) [blocked](javascript:alert(1))\n\n```rust\nfn main() { println!(\"ready\"); }\n```"}
      />,
    );

    expect(document.querySelector("script")).toBeNull();
    expect(document.querySelector("img")).toBeNull();
    expect(screen.getByText("<script>alert(1)</script>")).toBeTruthy();
    expect(screen.getByRole("list")).toBeTruthy();
    expect(screen.getByText("item").tagName).toBe("CODE");
    expect(screen.getByRole("heading", { name: "Result" }).tagName).toBe("H1");
    expect(screen.getByText("second").tagName).toBe("STRONG");
    expect(screen.getByText("stale").tagName).toBe("DEL");
    expect(screen.getByText("ready").tagName).toBe("EM");
    expect((screen.getByRole("checkbox") as HTMLInputElement).disabled).toBe(true);
    expect(screen.getByRole("table")).toBeTruthy();
    expect(document.querySelector(".hljs-keyword")?.textContent).toBe("fn");
    expect(screen.getByText("blocked").closest("a")).toBeNull();
    await user.click(screen.getByRole("link", { name: "Docs" }));
    expect(openExternalUrl).toHaveBeenCalledWith("https://example.com/docs");
    await user.click(screen.getByRole("button", { name: "Copy code" }));
    expect(writeText).toHaveBeenCalledWith("fn main() { println!(\"ready\"); }");

    if (originalClipboard === undefined) delete (navigator as { clipboard?: Clipboard }).clipboard;
    else Object.defineProperty(navigator, "clipboard", originalClipboard);
  });

  it("copies an admitted HTTPS link when the native opener is unavailable", async () => {
    const user = userEvent.setup();
    const originalClipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    render(
      <MessageContent
        text="[Docs](https://example.com/docs)"
        onOpenExternalUrl={async () => { throw new Error("native opener unavailable"); }}
      />,
    );

    await user.click(screen.getByRole("link", { name: "Docs" }));
    expect(writeText).toHaveBeenCalledWith("https://example.com/docs");

    if (originalClipboard === undefined) delete (navigator as { clipboard?: Clipboard }).clipboard;
    else Object.defineProperty(navigator, "clipboard", originalClipboard);
  });

  it("keeps reasoning collapsed and renders read-only bounded tool and diff surfaces", async () => {
    const user = userEvent.setup();
    const { unmount } = render(
      <Message message={{ key: "reasoning", kind: "reasoning", label: "Working", text: "private scratch", status: "details" }} />,
    );
    const disclosure = screen.getByText("Working").closest("details") as HTMLDetailsElement;
    expect(disclosure.open).toBe(false);
    expect(screen.getByText("Show details")).toBeTruthy();
    await user.click(screen.getByText("Show details"));
    expect(disclosure.open).toBe(true);
    expect(screen.getByText("Hide details")).toBeTruthy();
    unmount();

    const diff = "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-old\n+new";
    const diffRender = render(<DiffViewer diff={diff} />);
    expect(screen.getByLabelText("Unified diff")).toBeTruthy();
    const copyDiff = screen.getByRole("button", { name: "Copy diff" });
    expect(copyDiff.querySelector("svg")).toBeTruthy();
    expect(screen.queryByText("Copy diff")).toBeNull();
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

  it("presents successful structured tool output instead of a recorded placeholder", () => {
    const text = JSON.stringify({
      content: "pub fn ready() -> bool { true }\n",
      meta: { details: { call: { summary: "path=src/lib.rs" }, language: "rust" } },
      status: "ok",
    });
    const tool = { key: "read", toolName: "read_file", text };

    expect(presentTool(tool)).toMatchObject({
      displayName: "Read file",
      status: "Ok",
      tone: "success",
      summary: "pub fn ready() -> bool { true }",
      detailKind: "output",
      detailText: "pub fn ready() -> bool { true }",
      detailLanguage: "rust",
    });

    render(<ToolCard tool={tool} />);
    expect(screen.getByText("path=src/lib.rs")).toBeTruthy();
    expect(screen.getByText("View output")).toBeTruthy();
    expect(screen.getByText("Rust · 1 line")).toBeTruthy();
    expect(screen.getByLabelText("read_file output").querySelector(".hljs-keyword")?.textContent).toBe("pub");
    expect(screen.queryByText("Tool activity was recorded.")).toBeNull();
  });
});

describe("desktop workspace and history shell", () => {
  it("persists a theme switch and preserves the active conversation draft", async () => {
    const user = userEvent.setup();
    const setAppearance = vi.fn(async () => ({
      preference: "light" as const,
      resolvedTheme: "light" as const,
    }));
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
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
    await user.click(screen.getByRole("button", { name: "Open settings" }));
    await user.click(screen.getByRole("button", { name: "Light theme" }));

    await waitFor(() => expect(document.documentElement.dataset.theme).toBe("light"));
    expect(setAppearance).toHaveBeenCalledWith("light");
    await user.click(screen.getByLabelText("Sigil desktop home"));
    expect((screen.getByLabelText("Message Sigil") as HTMLTextAreaElement).value).toBe("Keep this draft");
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
    await user.click(screen.getByRole("button", { name: "Open settings" }));
    await user.click(screen.getByRole("button", { name: "Light theme" }));
    expect((await screen.findByRole("alert")).textContent).toContain("previous appearance is still active");
    expect(document.documentElement.dataset.theme).toBe("dark");

    await user.click(screen.getByRole("button", { name: "Retry" }));
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
    await userEvent.setup().click(screen.getByRole("button", { name: "Open settings" }));
    const systemTheme = screen.getByRole("button", { name: "System theme" });
    expect(systemTheme.getAttribute("aria-pressed")).toBe("true");
    act(() => listener?.({ preference: "system", resolvedTheme: "light" }));
    expect(document.documentElement.dataset.themePreference).toBe("system");
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(screen.getByRole("button", { name: "System theme" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("opens redacted support diagnostics from settings and saves through the native bridge", async () => {
    const user = userEvent.setup();
    const supportDoctor = vi.fn(async () => ({
      generatedAtUnixMs: 1_784_419_200_000,
      version: "0.0.1-alpha.5",
      commit: "test",
      target: "aarch64-apple-darwin",
      profile: "debug",
      environment: { os: "macos", architecture: "aarch64", terminalFamily: "other" },
      summary: { overallStatus: "warn" as const, ok: 4, warn: 1, error: 0 },
      checks: [{ status: "warn" as const, name: "configuration", summary: "Review one setting.", remediation: "Open sigil.toml." }],
      privacy: { included: ["build metadata"], excluded: ["credentials"], reviewBeforeSharing: true },
    }));
    const exportSupportBundle = vi.fn(async () => ({
      cancelled: false,
      fileName: "sigil-support-test.json",
    }));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      supportDoctor,
      exportSupportBundle,
    })} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "Open settings" }));
    await user.click(screen.getByRole("button", { name: "Open support and diagnostics" }));

    expect(await screen.findByRole("heading", { name: "Support & diagnostics" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Back to settings" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Open settings" }).getAttribute("aria-current")).toBe("page");
    expect(screen.getByRole("heading", { name: "Needs attention" })).toBeTruthy();
    expect(screen.getByText("Review one setting.")).toBeTruthy();
    expect(screen.getByText("credentials")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Save private report" }));
    await waitFor(() => expect(exportSupportBundle).toHaveBeenCalledWith(workspace.id));
    expect(await screen.findByText("Support report saved")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Back to settings" }));
    expect(await screen.findByRole("heading", { name: "Settings" })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Back to conversations" }));
    expect(await screen.findByRole("heading", { name: "Select a conversation" })).toBeTruthy();
  });

  it("persists a provider-validated default model for newly created desktop conversations", async () => {
    const user = userEvent.setup();
    let sequence = 0;
    const createSession = vi.fn(async () => ({
      id: `http-session-${++sequence}`,
      label: "New conversation",
      runCount: 0,
    }));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      createSession,
    })} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    await screen.findByRole("combobox", { name: "Model" });
    await user.click(screen.getByRole("button", { name: "Open settings" }));
    await user.selectOptions(
      await screen.findByRole("combobox", { name: "Default model for new conversations" }),
      "deepseek-v4-pro",
    );
    expect(window.localStorage.getItem("sigil.desktop.default-models.v1")).toContain("deepseek-v4-pro");
    await user.click(screen.getByRole("button", { name: "Back to conversations" }));
    await user.click(screen.getByRole("button", { name: "New conversation" }));

    await waitFor(() => expect(createSession).toHaveBeenCalledTimes(2));
    expect(createSession.mock.calls[1]).toEqual([workspace.id, "New conversation", "deepseek-v4-pro"]);
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

  it("finalizes reasoning labels without duplicating terminal state on the reply", () => {
    const base = {
      workspaceId: workspace.id,
      sessionId: "session-1",
      runId: "run-terminal",
      replayable: true,
    };
    const rows = reduceTimeline([
      { ...base, sequence: 1, kind: "run_started", text: "Hello" },
      { ...base, sequence: 2, kind: "reasoning_delta", text: "Inspecting" },
      { ...base, sequence: 3, kind: "assistant_message", text: "Hi" },
      { ...base, sequence: 4, kind: "run_finished", text: "Hi" },
    ]);

    expect(rows).toEqual([
      expect.objectContaining({ kind: "user", text: "Hello" }),
      expect.objectContaining({ kind: "reasoning", label: "Reasoning", text: "Inspecting" }),
      expect.objectContaining({ kind: "assistant", text: "Hi" }),
    ]);
    expect(rows.every((row) => row.status !== "complete")).toBe(true);
  });

  it("keeps streamed replies, tool calls, and the final answer in chronological order", () => {
    const base = {
      workspaceId: workspace.id,
      sessionId: "session-1",
      runId: "run-order",
      replayable: true,
    };
    const rows = reduceTimeline([
      { ...base, sequence: 1, kind: "run_started", text: "Inspect" },
      { ...base, sequence: 2, kind: "assistant_delta", text: "I will inspect." },
      { ...base, sequence: 3, kind: "assistant_message", text: "I will inspect.", assistantKind: "tool_preamble" },
      { ...base, sequence: 4, kind: "tool_started", itemId: "call-1", toolName: "bash", toolInput: "rg TODO", status: "running" },
      { ...base, sequence: 5, kind: "tool_result", itemId: "call-1", toolName: "bash", text: "1 match", status: "ok" },
      { ...base, sequence: 6, kind: "assistant_delta", text: "Found it." },
      { ...base, sequence: 7, kind: "assistant_message", text: "Found it.", assistantKind: "final_answer" },
      { ...base, sequence: 8, kind: "run_finished", text: "Found it." },
    ]);

    expect(rows.map((row) => row.kind)).toEqual(["user", "progress", "tool", "assistant"]);
    expect(rows.map((row) => row.text)).toEqual(["Inspect", "I will inspect.", "1 match", "Found it."]);
    expect(rows[2]).toEqual(expect.objectContaining({ input: "rg TODO" }));
  });

  it("replaces a waiting approval row with its resolved outcome", () => {
    const base = {
      workspaceId: workspace.id,
      sessionId: "session-1",
      runId: "run-approval",
      replayable: true,
      itemId: "call-1",
    };
    const rows = reduceTimeline([
      { ...base, sequence: 1, kind: "approval_requested", toolName: "bash", toolInput: "rg TODO" },
      { ...base, sequence: 2, kind: "approval_resolved", status: "approved", text: "Approved for this session" },
    ]);

    expect(rows).toEqual([expect.objectContaining({
      label: "Approved",
      status: "approved",
      text: "Approved for this session",
    })]);
  });

  it("hides low-level permission audit notices because approvals have dedicated rows", () => {
    const base = {
      workspaceId: workspace.id,
      sessionId: "session-1",
      runId: "run-permission",
      replayable: true,
    };
    const rows = reduceTimeline([
      { ...base, sequence: 1, kind: "notice", text: "permission read_file subject=README.md mode=allow" },
      { ...base, sequence: 2, kind: "notice", text: "permission write_file subject=README.md mode=ask" },
      { ...base, sequence: 3, kind: "notice", text: "permission bash subject=- mode=deny" },
    ]);

    expect(rows).toEqual([]);
  });

  it("uses the chronological durable run instead of appending its terminal replay backwards", () => {
    const transcript: TranscriptMessage[] = [
      {
        ordinal: 4,
        messageId: "final",
        role: "assistant",
        assistantKind: "final_answer",
        content: "Done",
        imageAttachmentCount: 0,
        truncated: false,
        originalContentBytes: 4,
      },
      {
        ordinal: 2,
        messageId: "tool",
        role: "tool",
        toolName: "read_file",
        content: "{\"content\":\"body\",\"status\":\"ok\"}",
        imageAttachmentCount: 0,
        truncated: false,
        originalContentBytes: 32,
      },
      {
        ordinal: 1,
        messageId: "user",
        role: "user",
        content: "Inspect",
        imageAttachmentCount: 0,
        truncated: false,
        originalContentBytes: 7,
      },
      {
        ordinal: 3,
        messageId: "reasoning",
        role: "assistant",
        assistantKind: "reasoning_trace",
        content: "Checking",
        imageAttachmentCount: 0,
        truncated: false,
        originalContentBytes: 8,
      },
    ];
    const base = {
      workspaceId: workspace.id,
      sessionId: "session-1",
      runId: "run-terminal",
      replayable: true,
    };
    const rows = reduceConversationTimeline(transcript, [
      { ...base, sequence: 1, kind: "run_started", text: "Inspect" },
      { ...base, sequence: 2, kind: "tool_result", itemId: "call-1", toolName: "read_file", text: "body", status: "ok" },
      { ...base, sequence: 3, kind: "run_finished", text: "Done" },
    ]);

    expect(rows.map((row) => [row.kind, row.text])).toEqual([
      ["user", "Inspect"],
      ["tool", "{\"content\":\"body\",\"status\":\"ok\"}"],
      ["reasoning", "Checking"],
      ["assistant", "Done"],
    ]);
  });

  it("omits empty durable tool preambles without hiding visible preamble text", () => {
    const base = {
      role: "assistant" as const,
      assistantKind: "tool_preamble" as const,
      imageAttachmentCount: 0,
      truncated: false,
      originalContentBytes: 0,
    };
    const rows = reduceConversationTimeline([
      { ...base, ordinal: 1, messageId: "empty-preamble" },
      {
        ...base,
        ordinal: 2,
        messageId: "visible-preamble",
        content: "I will inspect the affected file.",
        originalContentBytes: 33,
      },
      {
        ...base,
        ordinal: 3,
        messageId: "final",
        assistantKind: "final_answer",
        content: "Done.",
        originalContentBytes: 5,
      },
    ], []);

    expect(rows.map((row) => [row.status, row.text])).toEqual([
      ["tool preamble", "I will inspect the affected file."],
      [undefined, "Done."],
    ]);
  });

  it("restores the most recent workspace after native bootstrap", async () => {
    const openRecentWorkspace = vi.fn(async () => workspace);
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
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
        protocolVersion: 2,
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
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
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
    expect(await screen.findByText("0 recorded runs")).toBeTruthy();
    expect(screen.queryByText("New conversation ready.")).toBeNull();
    expect(document.querySelector(".sg-notification-viewport")).toBeNull();
    expect(document.querySelector(".session-notice")).toBeNull();
    expect(screen.getAllByRole("heading", { name: "New conversation" })).toHaveLength(1);
    expect(screen.getByRole("complementary", { name: "Conversation navigation" })).toBeTruthy();
    expect(screen.getByRole("region", { name: "Conversation workspace" })).toBeTruthy();
    expect(screen.queryByRole("complementary", { name: "Verification" })).toBeNull();
    expect(screen.queryByRole("button", { name: /Open verification:/ })).toBeNull();
    expect(document.querySelector(".conversation-layout-with-review")).toBeNull();
    expect(screen.queryByText(/private bearer|TUI-first|stay in Rust/i)).toBeNull();

    await user.click(screen.getByRole("button", { name: "Switch workspace: sigil" }));
    const workspaceDialog = screen.getByRole("dialog", { name: "Switch workspace: sigil" });
    expect(workspaceDialog.querySelector(".workspace-switcher-content")).toBeTruthy();
    expect(workspaceDialog.querySelector(".workspace-switcher-panel")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Close sigil" }));
    await waitFor(() => expect(closeRequest).toBe(workspace.id));
    expect(document.querySelector(".sg-notification-viewport")).toBeNull();
  });

  it("pages generation-consistent history and opens only a ready durable entry", async () => {
    const user = userEvent.setup();
    const cursors: Array<string | undefined> = [];
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
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
                  sourceBytes: 1024,
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
                  sourceBytes: 256,
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

  it("keeps the active conversation selected and ignores repeated opens of the same session", async () => {
    const user = userEvent.setup();
    const openSession = vi.fn(async () => ({ id: "adapter-session", label: "Active session", runCount: 0 }));
    const entry = {
      sessionRef: "active.jsonl",
      sessionId: "durable-active",
      sourceState: "ready" as const,
      sourceBytes: 512,
      sourceModifiedAtUnixMs: 1_784_419_200_000,
      title: "Active session",
      userMessageCount: 0,
      assistantMessageCount: 0,
      toolResultCount: 0,
      pinned: false,
    };
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      catalog: async () => ({ ...emptyCatalog, entries: [entry] }),
      openSession,
    })} />);

    const row = await screen.findByRole("button", { name: /^Active session/ });
    await user.click(row);
    await screen.findByRole("heading", { name: "Active session" });
    await waitFor(() => expect(row.getAttribute("aria-current")).toBe("page"));
    await user.click(row);
    await new Promise((resolve) => window.setTimeout(resolve, 300));

    expect(openSession).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("status", { name: "Opening conversation…" })).toBeNull();
  });

  it("previews and executes selected conversation mutations through the batch contract", async () => {
    const user = userEvent.setup();
    const entry = {
      sessionRef: "batch-ready.jsonl",
      sessionId: "durable-batch-ready",
      sourceState: "ready" as const,
      sourceBytes: 1024,
      sourceModifiedAtUnixMs: 1_784_419_200_000,
      providerName: "deepseek",
      modelName: "deepseek-chat",
      title: "Batch ready session",
      userMessageCount: 2,
      assistantMessageCount: 2,
      toolResultCount: 1,
      pinned: false,
    };
    const planSessionCatalogBatch = vi.fn(async () => ({
      planId: "sha256:batch-ready",
      action: "delete_sessions" as const,
      generation: 7,
      total: 1,
      executable: 1,
      blocked: 0,
      items: [{ sessionRef: entry.sessionRef, status: "executable" as const }],
    }));
    const executeSessionCatalogBatch = vi.fn(async () => ({
      planId: "sha256:batch-ready",
      action: "delete_sessions" as const,
      total: 1,
      completed: 1,
      failed: 0,
      skipped: 0,
      items: [{ sessionRef: entry.sessionRef, outcome: "completed" as const }],
    }));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async () => ({ ...emptyCatalog, generation: 7, entries: [entry], nextCursor: "cursor-2" }),
      planSessionCatalogBatch,
      executeSessionCatalogBatch,
    })} />);

    await screen.findByText("Batch ready session");
    fireEvent.click(screen.getByRole("button", { name: "Manage conversations" }));
    const heading = await screen.findByRole("heading", { name: "Conversation Library" });
    const backButton = screen.getByRole("button", { name: "Back to conversations" });
    expect(heading.closest(".application-page-toolbar")?.contains(backButton)).toBe(true);
    expect(screen.getByRole("button", { name: "Manage conversations" }).getAttribute("aria-current")).toBe("page");
    const loadMore = screen.getByRole("button", { name: "Load more" });
    expect(loadMore.closest(".library-table-frame")).toBeTruthy();
    expect(loadMore.closest(".library-pagination")).toBeTruthy();
    await user.click(screen.getByRole("checkbox", { name: "Select conversation: Batch ready session" }));
    await user.click(screen.getByRole("button", { name: "Delete ready (1)" }));

    expect(await screen.findByRole("dialog", { name: "Review batch action" })).toBeTruthy();
    expect(planSessionCatalogBatch).toHaveBeenCalledWith(workspace.id, {
      action: "delete_sessions",
      items: [{ sessionRef: entry.sessionRef, sessionId: entry.sessionId }],
    });
    await user.click(screen.getByRole("button", { name: "Apply action" }));
    await waitFor(() => expect(executeSessionCatalogBatch).toHaveBeenCalledWith(workspace.id, {
      planId: "sha256:batch-ready",
      action: "delete_sessions",
      items: [{ sessionRef: entry.sessionRef, sessionId: entry.sessionId }],
    }));
    expect(await screen.findByRole("heading", { name: "Latest batch result" })).toBeTruthy();
  });

  it("stores the startup restore preference and honors it on the next bootstrap", async () => {
    const user = userEvent.setup();
    const openRecentWorkspace = vi.fn(async () => workspace);
    const recent = [{ id: workspace.id, displayName: workspace.displayName, isOpen: false }];
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [], recentWorkspaces: recent }),
      openRecentWorkspace,
    })} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "Open settings" }));
    await user.click(screen.getByRole("checkbox", { name: "Reopen the most recent workspace" }));
    expect(window.localStorage.getItem("sigil.desktop.reopen-last-workspace.v1")).toBe("false");

    cleanup();
    openRecentWorkspace.mockClear();
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [], recentWorkspaces: recent }),
      openRecentWorkspace,
    })} />);
    await screen.findByRole("heading", { name: "Open a workspace" });
    expect(openRecentWorkspace).not.toHaveBeenCalled();
  });

  it("shows branded conversation loading in the workspace instead of the session rail", async () => {
    const user = userEvent.setup();
    let resolveOpen: ((session: SessionSummary) => void) | undefined;
    let resolveTranscript: ((page: { totalMessages: number; messages: []; }) => void) | undefined;
    let resolveRunContext: ((context: RunContext) => void) | undefined;
    const transcript = vi.fn(() => new Promise<{ totalMessages: number; messages: []; }>((resolve) => {
      resolveTranscript = resolve;
    }));
    const runContext = vi.fn(() => new Promise<RunContext>((resolve) => {
      resolveRunContext = resolve;
    }));
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "loading.jsonl",
          sessionId: "durable-loading",
          sourceState: "ready",
          sourceBytes: 1024,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Loading state session",
          userMessageCount: 1,
          assistantMessageCount: 1,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: () => new Promise((resolve) => {
        resolveOpen = resolve;
      }),
      transcript,
      runContext,
    });
    render(<App bridge={bridge} />);

    const sessionButton = await screen.findByRole("button", { name: /^Loading state session/ });
    await user.click(sessionButton);
    const workspaceRegion = screen.getByRole("region", { name: "Conversation workspace" });
    const navigation = screen.getByRole("complementary", { name: "Conversation navigation" });
    const loading = await within(workspaceRegion).findByRole("status", { name: "Opening conversation…" });
    expect(within(workspaceRegion).getByText(/Restoring Loading state session/)).toBeTruthy();
    expect(within(navigation).queryByText("Opening conversation…")).toBeNull();
    expect((sessionButton as HTMLButtonElement).disabled).toBe(true);

    await act(async () => {
      resolveOpen?.({ id: "http-session-loading", label: "Loading state session", runCount: 2 });
    });
    await waitFor(() => expect(transcript).toHaveBeenCalledOnce());
    await waitFor(() => expect(runContext).toHaveBeenCalledOnce());
    expect(within(workspaceRegion).getByRole("status", { name: "Opening conversation…" })).toBe(loading);

    await act(async () => {
      resolveTranscript?.({ totalMessages: 0, messages: [] });
    });
    expect(within(workspaceRegion).getByRole("status", { name: "Opening conversation…" })).toBe(loading);

    await act(async () => {
      resolveRunContext?.(defaultRunContext);
    });
    expect(await within(workspaceRegion).findByRole("heading", { name: "Loading state session" })).toBeTruthy();
    expect(within(workspaceRegion).queryByRole("status", { name: "Opening conversation…" })).toBeNull();
    expect((within(workspaceRegion).getByRole("combobox", { name: "Model" }) as HTMLSelectElement).value).toBe("deepseek-v4-flash");
    expect(within(workspaceRegion).getByRole("combobox", { name: "Reasoning effort" })).toBeTruthy();
    expect(within(workspaceRegion).getByRole("meter", { name: "Context usage 3%" })).toBeTruthy();
  });

  it("keeps the conversation rail stable with row skeletons while history loads", async () => {
    let resolveCatalog: ((page: CatalogPage) => void) | undefined;
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: () => new Promise((resolve) => {
        resolveCatalog = resolve;
      }),
    });
    render(<App bridge={bridge} />);

    const navigation = await screen.findByRole("complementary", { name: "Conversation navigation" });
    expect(await within(navigation).findByRole("status", { name: "Loading conversations…" })).toBeTruthy();
    expect(navigation.querySelectorAll(".history-skeleton-row")).toHaveLength(6);

    await act(async () => {
      resolveCatalog?.(emptyCatalog);
    });
    expect(await within(navigation).findByText("No matching conversation.")).toBeTruthy();
    expect(navigation.querySelector(".history-skeleton")).toBeNull();
  });

  it("opens bounded transcript text and pages older messages in chronological order", async () => {
    const user = userEvent.setup();
    const transcriptQueries: Array<number | undefined> = [];
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
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
            sourceBytes: 1024,
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
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "active.jsonl",
          sessionId: "durable-active",
          sourceState: "ready",
          sourceBytes: 1024,
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
            permissionMode: "manual",
            streamSequence: 2,
          },
          events: [activeEvent, approvalEvent],
          streamState: "live",
          hasGap: true,
        };
      },
      cancelRun: async (_workspaceId, sessionId, runId) => {
        cancelledRun = runId;
        return { id: runId, sessionId, status: "cancel_requested", permissionMode: "manual", streamSequence: 3 };
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
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "keep.jsonl",
          sessionId: "durable-keep",
          sourceState: "ready",
          sourceBytes: 1024,
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
    const clearActiveFilters = screen.getByRole("button", { name: "Clear" });
    expect(clearActiveFilters.querySelector("svg")).toBeTruthy();
    expect(screen.queryByText("Clear")).toBeNull();
    await user.click(clearActiveFilters);
    expect(screen.queryByText("1 active filter")).toBeNull();
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
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "managed.jsonl",
          sessionId: "durable-managed",
          sourceState: "ready",
          sourceBytes: 1024,
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
    const managedRow = screen.getByRole("button", { name: /^Managed conversation/ });
    await user.click(managedRow);
    await user.click(managedRow);
    expect(screen.getByRole("dialog", { name: "Rename conversation" })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Cancel" }));

    fireEvent.contextMenu(managedRow);
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
    const deleted = await screen.findByText("Conversation deleted.");
    expect(deleted.closest(".sg-toast-success")).toBeTruthy();
    expect(document.querySelector(".session-notice")).toBeNull();
  });

  it("quarantines an invalid source from its context menu after confirmation", async () => {
    const quarantineSession = vi.fn(async (_workspaceId, input: { sessionRef: string }) => ({
      sessionRef: input.sessionRef,
      quarantineName: `quarantined--${input.sessionRef}`,
      projectionGeneration: 2,
    }));
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async () => ({
        ...emptyCatalog,
        degradedSourceCount: 1,
        entries: [{
          sessionRef: "broken.jsonl",
          sourceState: "invalid",
          sourceBytes: 17,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Broken source",
          userMessageCount: 0,
          assistantMessageCount: 0,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      quarantineSession,
    });
    const user = userEvent.setup();
    render(<App bridge={bridge} />);

    const unavailableRow = await screen.findByText("Broken source");
    fireEvent.contextMenu(unavailableRow);
    await user.click(screen.getByRole("menuitem", { name: "Move invalid source to quarantine" }));
    expect(screen.getByRole("alertdialog", { name: "Quarantine invalid conversation source?" })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Move to quarantine" }));
    await waitFor(() => expect(quarantineSession).toHaveBeenCalledWith(workspace.id, {
      sessionRef: "broken.jsonl",
      sourceBytes: 17,
      sourceModifiedAtUnixMs: 1_784_419_200_000,
    }));
    expect((await screen.findByText("Invalid conversation source moved to quarantine.")).closest(".sg-toast-success")).toBeTruthy();
  });

  it("permanently deletes an invalid source only after explicit confirmation", async () => {
    const deleteInvalidSessionSource = vi.fn(async (_workspaceId, input: { sessionRef: string }) => ({
      sessionRef: input.sessionRef,
      projectionGeneration: 3,
    }));
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async () => ({
        ...emptyCatalog,
        degradedSourceCount: 1,
        entries: [{
          sessionRef: "broken.jsonl",
          sourceState: "invalid",
          sourceBytes: 17,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Broken source",
          userMessageCount: 0,
          assistantMessageCount: 0,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      deleteInvalidSessionSource,
    });
    const user = userEvent.setup();
    render(<App bridge={bridge} />);

    const unavailableRow = await screen.findByText("Broken source");
    fireEvent.contextMenu(unavailableRow);
    await user.click(screen.getByRole("menuitem", { name: "Delete invalid source" }));
    expect(deleteInvalidSessionSource).not.toHaveBeenCalled();
    expect(screen.getByRole("alertdialog", { name: "Delete invalid conversation source?" })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Delete permanently" }));
    await waitFor(() => expect(deleteInvalidSessionSource).toHaveBeenCalledWith(workspace.id, {
      sessionRef: "broken.jsonl",
      sourceBytes: 17,
      sourceModifiedAtUnixMs: 1_784_419_200_000,
    }));
  });

  it("restores the unfiltered conversation list as soon as search text is cleared", async () => {
    const queries: Array<string | undefined> = [];
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async (_workspaceId, request) => {
        queries.push(request.query);
        return emptyCatalog;
      },
    });
    const user = userEvent.setup();
    render(<App bridge={bridge} />);

    const search = await screen.findByRole("textbox", { name: "Search conversations" });
    await user.type(search, "needle");
    await user.click(screen.getByRole("button", { name: "Search" }));
    await waitFor(() => expect(queries.at(-1)).toBe("needle"));
    await user.clear(search);
    await waitFor(() => expect(queries.at(-1)).toBeUndefined());
  });

  it("requires explicit confirmation before closing a workspace with active runs", async () => {
    const user = userEvent.setup();
    const confirmations: boolean[] = [];
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
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
    await waitFor(() => expect(confirmations).toEqual([false, false, true]));
    expect(document.querySelector(".sg-notification-viewport")).toBeNull();
  });

  it("uses primitive navigation and contextual review drawers without remounting the conversation", async () => {
    const restoreMedia = installMediaQueries((query) => query.includes("max-width"));
    const user = userEvent.setup();
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
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
    await waitFor(() => expect(screen.queryByLabelText("Creating a new conversation…")).toBeNull());
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

  it("shows durable child Agent activity and confirms its result returned to the conversation", async () => {
    const user = userEvent.setup();
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      agentActivity: async () => ({
        totalAgents: 1,
        activeAgents: 0,
        terminalAgents: 1,
        items: [{
          threadId: "agent_review",
          profileId: "explore",
          displayName: "Repository review",
          objective: "Inspect the repository architecture",
          status: "completed",
          handoffStatus: "returned",
          resultSummary: "The child Agent returned a bounded architecture report.",
          resultSummaryTruncated: false,
          usage: {
            inputTokens: 240,
            outputTokens: 80,
            totalTokens: 320,
          },
        }],
      }),
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    const activityTrigger = await screen.findByRole("button", { name: "Open Agent activity" });
    await user.click(activityTrigger);

    expect(screen.getByRole("dialog", { name: "Agent activity" })).toBeTruthy();
    expect(screen.getByText("Repository review")).toBeTruthy();
    expect(screen.getByText("Completed")).toBeTruthy();
    expect(screen.getByText("Returned to conversation")).toBeTruthy();
    expect(screen.getByText("The child Agent returned a bounded architecture report.")).toBeTruthy();
    expect(screen.getByText("320 tokens")).toBeTruthy();
  });

  it("automatically restarts stale pagination instead of mixing generations", async () => {
    const user = userEvent.setup();
    let firstPageRequests = 0;
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async (_workspaceId, request) => {
        if (request.cursor !== undefined) throw { code: "catalog_stale" };
        firstPageRequests += 1;
        return { ...emptyCatalog, nextCursor: "stale-cursor" };
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "Load more" }));
    await waitFor(() => expect(firstPageRequests).toBe(2));
    expect(screen.queryByText("Conversation history refreshed because the list changed.")).toBeNull();
    expect(screen.queryByText("History changed while paging.")).toBeNull();
    expect(document.querySelector(".sg-notification-viewport")).toBeNull();
  });

  it("runs a prompt and merges streamed and durable completion into one assistant reply", async () => {
    const user = userEvent.setup();
    let eventListener: ((event: TimelineEvent) => void) | undefined;
    let statusListener: ((status: RunStreamStatus) => void) | undefined;
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
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
    expect(await screen.findByText("Available")).toBeTruthy();
    await user.type(screen.getByLabelText("Message Sigil"), "Say hello");
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(screen.getByText("Say hello")).toBeTruthy();
    expect(await screen.findByRole("button", { name: "Stop run" })).toBeTruthy();
    expect(screen.queryByText("Starting the run…")).toBeNull();
    expect(screen.queryByText("Run started. Live updates are connected.")).toBeNull();
    expect(document.querySelector(".sg-notification-viewport")).toBeNull();
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
    expect(screen.queryByText("complete")).toBeNull();
    expect(screen.getByText("Completed")).toBeTruthy();
    expect(screen.getByText("Run finished. Review the final response and verification status.")).toBeTruthy();
  });

  it("preserves IME text, accepts clipboard input, and does not submit during composition", async () => {
    const user = userEvent.setup();
    const prompts: string[] = [];
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      startRun: async (_workspaceId, sessionId, prompt) => {
        prompts.push(prompt);
        return { id: "run-ime", sessionId, status: "running", permissionMode: "manual", streamSequence: 0 };
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

  it("projects model, effort, and context facts and sends exact run controls", async () => {
    const user = userEvent.setup();
    let selectedMode = "";
    let selectedEffort = "";
    let selectedEffortBinding = "";
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      startRun: async (
        _workspaceId,
        sessionId,
        _prompt,
        permissionMode,
        _modelName,
        _modelSelectionBinding,
        reasoningEffort,
        reasoningEffortBinding,
      ) => {
        selectedMode = permissionMode;
        selectedEffort = reasoningEffort ?? "";
        selectedEffortBinding = reasoningEffortBinding ?? "";
        return {
          id: "run-mode",
          sessionId,
          status: "running",
          permissionMode,
          streamSequence: 0,
        };
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    expect((await screen.findByRole("combobox", { name: "Model" }) as HTMLSelectElement).value).toBe("deepseek-v4-flash");
    expect(screen.getByRole("meter", { name: "Context usage 3%" })).toBeTruthy();
    await user.selectOptions(screen.getByRole("combobox", { name: "Permission mode" }), "read-only");
    await user.selectOptions(screen.getByRole("combobox", { name: "Reasoning effort" }), "high");
    const composer = screen.getByLabelText("Message Sigil") as HTMLTextAreaElement;
    Object.defineProperty(composer, "scrollHeight", { configurable: true, value: 240 });
    await user.type(composer, "Inspect safely");
    expect(composer.style.height).toBe("176px");
    expect(composer.style.overflowY).toBe("auto");
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(selectedMode).toBe("read-only");
    expect(selectedEffort).toBe("high");
    expect(selectedEffortBinding).toBe("effort-binding-deepseek-v4-flash");
  });

  it("selects a model for the next run without creating another conversation", async () => {
    const user = userEvent.setup();
    const selectedModels: Array<string | undefined> = [];
    const runSelections: Array<{
      sessionId: string;
      modelName?: string;
      binding?: string;
      effort?: string;
      effortBinding?: string;
    }> = [];
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      createSession: async (_workspaceId, _label, modelName) => {
        selectedModels.push(modelName);
        return {
          id: `http-session-${selectedModels.length}`,
          label: "New conversation",
          runCount: 0,
        };
      },
      startRun: async (
        _workspaceId,
        sessionId,
        _prompt,
        permissionMode,
        modelName,
        modelSelectionBinding,
        reasoningEffort,
        reasoningEffortBinding,
      ) => {
        runSelections.push({
          sessionId,
          modelName,
          binding: modelSelectionBinding,
          effort: reasoningEffort,
          effortBinding: reasoningEffortBinding,
        });
        return {
          id: "run-model-switch",
          sessionId,
          status: "running",
          permissionMode,
          reasoningEffort,
          streamSequence: 0,
        };
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    await user.selectOptions(
      await screen.findByRole("combobox", { name: "Model" }),
      "deepseek-v4-pro",
    );
    expect((screen.getByRole("combobox", { name: "Reasoning effort" }) as HTMLSelectElement).value).toBe("max");
    expect(screen.queryByRole("option", { name: "Effort unavailable" })).toBeNull();
    await user.type(screen.getByLabelText("Message Sigil"), "Continue with pro");
    await user.click(screen.getByRole("button", { name: "Send message" }));

    expect(selectedModels).toEqual([undefined]);
    expect(runSelections).toEqual([{
      sessionId: "http-session-1",
      modelName: "deepseek-v4-pro",
      binding: "model-binding-deepseek-v4-flash",
      effort: "max",
      effortBinding: "effort-binding-deepseek-v4-pro",
    }]);
  });

  it("switches and persists the desktop language", async () => {
    const user = userEvent.setup();
    window.localStorage.removeItem("sigil.desktop.locale.v1");
    render(<App bridge={bridgeWith()} />);

    await screen.findByRole("heading", { name: "Open a workspace" });
    await user.click(screen.getByRole("button", { name: "Open settings" }));
    await user.click(screen.getByRole("button", { name: "简体中文" }));

    expect(screen.getByRole("heading", { name: "设置" })).toBeTruthy();
    expect(document.documentElement.lang).toBe("zh-CN");
    expect(window.localStorage.getItem("sigil.desktop.locale.v1")).toBe("zh-CN");

    await user.click(screen.getByRole("button", { name: "English" }));
    expect(document.documentElement.lang).toBe("en");
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
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      startRun: async (_workspaceId, sessionId, prompt) => {
        prompts.push(prompt);
        return { id: "run-draft", sessionId, status: "running", permissionMode: "manual", streamSequence: 0 };
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
        protocolVersion: 2,
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

    timeline.scrollTop = 0;
    act(() => {
      eventListener?.({ ...base, sequence: 2, kind: "assistant_delta", text: "Growing" });
    });
    expect(timeline.scrollTop).toBe(600);

    timeline.scrollTop = 100;
    fireEvent.scroll(timeline);
    act(() => {
      eventListener?.({ ...base, sequence: 3, kind: "assistant_message", text: "Second" });
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
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      subscribeRunEvents: async (listener) => {
        eventListener = listener;
        return () => undefined;
      },
      resolveApproval: async (_workspaceId, _sessionId, runId, approval, decision) => {
        approvedCall = `${runId}:${approval.approvalRequestId}:${decision}`;
        return { runId, callId: approval.callId, decision: decision === "deny" ? "denied" : "approved" };
      },
      cancelRun: async (_workspaceId, sessionId, runId) => {
        cancelledRun = runId;
        return { id: runId, sessionId, status: "cancel_requested", permissionMode: "manual", streamSequence: 4 };
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
          sessionGrantAvailable: true,
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
    await user.click(screen.getByRole("button", { name: "Allow for session" }));
    expect(approvedCall).toBe("run-1:approval-1:approve_session");
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
    await waitFor(() => expect(cancelledRun).toBe("run-1"));
    expect(screen.queryByText("Requesting cancellation…")).toBeNull();
    expect(screen.queryByText("Cancellation requested. Waiting for the run to stop safely.")).toBeNull();
    expect(document.querySelector(".sg-notification-viewport")).toBeNull();
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
        protocolVersion: 2,
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
    const copyReceipt = screen.getByRole("button", { name: "Copy receipt" });
    expect(copyReceipt.querySelector("svg")).toBeTruthy();
    expect(screen.queryByText(/^Copy$/)).toBeNull();
    await user.click(screen.getByRole("button", { name: "Run recommended check" }));
    expect(await screen.findByText("passed")).toBeTruthy();
    expect(rerunSnapshot).toBe("snapshot-1");
  });

  it("renders dismiss-only errors as compact accessible icon actions", async () => {
    const user = userEvent.setup();
    const onDismiss = vi.fn();
    render(
      <ErrorCard
        title="Run action needs attention"
        message="The action could not be completed."
        actionLabel="Dismiss"
        actionIcon={<Icon name="close" />}
        onAction={onDismiss}
      />,
    );

    const dismiss = screen.getByRole("button", { name: "Dismiss" });
    expect(dismiss.querySelector("svg")).toBeTruthy();
    expect(screen.queryByText("Dismiss")).toBeNull();
    await user.click(dismiss);
    expect(onDismiss).toHaveBeenCalledOnce();
  });
});
