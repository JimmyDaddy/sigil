import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { App } from "./App";
import type { AppearanceSnapshot } from "./appearance/contract";
import { DiffViewer } from "./DiffViewer";
import { ErrorCard } from "./ErrorCard";
import { Message } from "./Message";
import { MessageContent } from "./MessageContent";
import { presentTool, ToolCard } from "./ToolCard";
import type { DesktopBridge } from "./bridge";
import type {
  CatalogPage,
  ConversationDisplayPage,
  ConversationContinuity,
  ConversationQueueCommandInput,
  DesktopBootstrap,
  PermissionMode,
  RunContext,
  RunStreamStatus,
  ProviderSetupCatalog,
  SessionSummary,
  TaskIntegrationReview,
  TimelineEvent,
  ToolArtifactPage,
  ToolArtifactSelector,
  WorkspaceSummary,
} from "./types";
import { Icon } from "./ui/icons";
import { readLastSession, writeLastSession } from "./preferences";

const originalMatchMedia = Object.getOwnPropertyDescriptor(window, "matchMedia");

afterEach(() => {
  cleanup();
  window.localStorage.clear();
  vi.restoreAllMocks();
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
  resolvedTheme: "sigil_dark",
};

const defaultRunContext: RunContext = {
  modelRef: {
    connectionId: "deepseek-default",
    modelId: "deepseek-v4-flash",
  },
  providerName: "deepseek",
  modelName: "deepseek-v4-flash",
  modelOptions: [
    {
      modelRef: { connectionId: "deepseek-default", modelId: "deepseek-v4-flash" },
      displayName: "DeepSeek V4 Flash",
      availability: "available",
      recommendation: "recommended",
      provenance: "bundled",
      modelName: "deepseek-v4-flash",
      availableReasoningEfforts: ["low", "medium", "high", "max"],
      defaultReasoningEffort: "max",
      reasoningEffortBinding: "effort-binding-deepseek-v4-flash",
    },
    {
      modelRef: { connectionId: "deepseek-default", modelId: "deepseek-v4-pro" },
      displayName: "DeepSeek V4 Pro",
      availability: "available",
      recommendation: "standard",
      provenance: "bundled",
      modelName: "deepseek-v4-pro",
      availableReasoningEfforts: ["low", "medium", "high", "max"],
      defaultReasoningEffort: "max",
      reasoningEffortBinding: "effort-binding-deepseek-v4-pro",
    },
  ],
  modelSelection: "same_session",
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
  const {
    bootstrap,
    startRun,
    continueTask,
    continuity,
    attachRun,
    ...remainingOverrides
  } = overrides;
  let simulatedOwner: ConversationContinuity["foregroundOwner"];
  return {
    bootstrap: async () => ({
      appearance: defaultAppearance,
      ...(bootstrap === undefined
        ? { protocolVersion: 2, workspaces: [], recentWorkspaces: [] }
        : await bootstrap()),
    }),
    updateState: async () => ({
      phase: "unsupported",
      channel: "beta",
      currentVersion: "0.0.1-alpha.6",
      downloadedBytes: 0,
    }),
    checkForUpdate: async () => ({
      phase: "unsupported",
      channel: "beta",
      currentVersion: "0.0.1-alpha.6",
      downloadedBytes: 0,
    }),
    downloadAndInstallUpdate: async () => ({
      phase: "unsupported",
      channel: "beta",
      currentVersion: "0.0.1-alpha.6",
      downloadedBytes: 0,
    }),
    restartAfterUpdate: async () => undefined,
    setAppearance: async (preference) => ({
      preference,
      resolvedTheme: preference === "system" ? "sigil_dark" : preference,
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
    providerConnections: async () => ({
      configMode: "v2",
      defaultModel: { connectionId: "deepseek-default", modelId: "deepseek-v4-flash" },
      connections: [{
        id: "deepseek-default",
        label: "DeepSeek",
        providerLabel: "DeepSeek",
        protocolLabel: "DeepSeek",
        endpointDisplay: "api.deepseek.com",
        credentialSource: "environment",
        readiness: "ready",
        defaultModel: { connectionId: "deepseek-default", modelId: "deepseek-v4-flash" },
      }],
      issues: [],
    }),
    providerSetupCatalog: async () => ({
      connectionId: "deepseek-1",
      providerLabel: "DeepSeek",
      state: "remote",
      models: [{
        modelId: "deepseek-v4-flash",
        displayName: "DeepSeek V4 Flash",
        availability: "available",
        recommended: true,
        provenance: "remote",
      }],
      suggestedModel: "deepseek-v4-flash",
      manualEntryAllowed: false,
    }),
    saveProviderSetup: async () => ({
      defaultModel: { connectionId: "deepseek-1", modelId: "deepseek-v4-flash" },
      inventory: {
        configMode: "v2",
        defaultModel: { connectionId: "deepseek-1", modelId: "deepseek-v4-flash" },
        connections: [{
          id: "deepseek-1",
          label: "DeepSeek 1",
          providerLabel: "DeepSeek",
          protocolLabel: "DeepSeek",
          endpointDisplay: "api.deepseek.com",
          credentialSource: "stored",
          readiness: "ready",
          defaultModel: { connectionId: "deepseek-1", modelId: "deepseek-v4-flash" },
        }],
        issues: [],
      },
      saveWarning: false,
    }),
    saveProviderDefaultModel: async (_workspaceId, modelRef) => ({
      defaultModel: modelRef,
      inventory: {
        configMode: "v2",
        defaultModel: modelRef,
        connections: [{
          id: modelRef.connectionId,
          label: "DeepSeek",
          providerLabel: "DeepSeek",
          protocolLabel: "DeepSeek",
          endpointDisplay: "api.deepseek.com",
          credentialSource: "environment",
          readiness: "ready",
          defaultModel: modelRef,
        }],
        issues: [],
      },
      saveWarning: false,
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
    display: async () => ({
      schemaVersion: 1,
      requestScope: "test-request-scope",
      throughSessionStreamSequence: "0",
      totalItems: "0",
      items: [],
      hasMore: false,
      gapFacts: [],
    }),
    readToolArtifact: async (_workspaceId, sessionId, input) => ({
      schemaVersion: 1,
      requestScope: sessionId,
      artifactRef: input.artifactRef,
      selector: input.selector,
      body: "",
      bodyEncoding: "utf8",
      returnedBytes: 0,
      pageSha256: `sha256:${"0".repeat(64)}`,
      artifactSha256: `sha256:${"0".repeat(64)}`,
      eof: true,
      matchCount: 0,
    }),
    continuity: continuity ?? (async () => ({
      durableFrontier: { throughStreamSequence: 0 },
      foregroundOwner: simulatedOwner,
      retainedTerminalRuns: [],
      recoveryActions: [],
    })),
    conversationQueue: async (_workspaceId, sessionId) => ({
      schemaVersion: 1,
      sessionId,
      generation: "0",
      paused: false,
      totalItems: 0,
      items: [],
      truncated: false,
    }),
    commandConversationQueue: async (_workspaceId, input) => ({
      commandId: "queue-command-test",
      clientId: "sigil-desktop",
      sessionId: input.sessionId,
      action: input.action.action,
      expectedGeneration: input.expectedGeneration,
      generation: input.expectedGeneration,
      queue: {
        schemaVersion: 1,
        sessionId: input.sessionId,
        generation: input.expectedGeneration,
        paused: input.action.action === "pause",
        totalItems: 0,
        items: [],
        truncated: false,
      },
      replayed: false,
    }),
    conversationRecovery: async () => ({ checkpoints: [], forkPoints: [], throughStreamSequence: 0 }),
    conversationCompactionPreview: async () => ({
      previewId: "compaction-preview-test",
      foldedEventCount: 4,
      retainedEventCount: 2,
      admission: {
        kind: "ready",
        economics: {
          beforeInputTokens: 200,
          targetInputTokens: 100,
          contextWindowTokens: 1_000,
          outputTokens: 20,
          safetyBufferTokens: 10,
          savingsTokens: 100,
          savingsRatioPpm: 500_000,
          minimumSavingsTokens: 10,
          minimumSavingsRatioPpm: 10_000,
          summaryCacheReadTokens: 80,
          summaryUncachedInputTokens: 20,
          summaryOutputTokens: 8,
          summaryCostNanoUsd: 12,
        },
      },
    }),
    compactConversation: async () => ({
      outcome: "applied",
      recovery: { checkpoints: [], forkPoints: [], throughStreamSequence: 1 },
      compaction: {
        compactionId: "compaction-test",
        attemptId: "attempt-test",
        taskMemoryId: "task-memory-test",
        foldedEventCount: 4,
        toolOutputProjectionRecorded: true,
        nativeCarrierMaterialized: false,
      },
    }),
    checkpointRestorePreview: async (_workspaceId, input) => ({
      checkpointId: input.checkpointId,
      checkpointDigest: input.checkpointDigest,
      files: [],
      reverseDiffs: [],
      unknownMutationCount: 0,
      ready: true,
    }),
    commandConversationRecovery: async (_workspaceId, input) => ({
      commandId: "recovery-command-test",
      clientId: "sigil-desktop",
      sessionId: input.sessionId,
      action: input.action.kind,
      recovery: { checkpoints: [], forkPoints: [], throughStreamSequence: 0 },
      replayed: false,
    }),
    runContext: async () => defaultRunContext,
    agentActivity: async () => ({
      totalAgents: 0,
      activeAgents: 0,
      terminalAgents: 0,
      items: [],
    }),
    startRun: async (...args) => {
      const started = startRun === undefined
        ? {
            id: "run-1",
            sessionId: args[1],
            status: "running" as const,
            permissionMode: "manual" as const,
            streamSequence: 0,
          }
        : await startRun(...args);
      simulatedOwner = {
        runId: started.id,
        ownerRevision: `sha256:${"1".repeat(64)}`,
      };
      return started;
    },
    continueTask: async (...args) => {
      const started = continueTask === undefined
        ? {
            id: "run-task-continue",
            sessionId: args[1],
            status: "running" as const,
            permissionMode: args[3],
            streamSequence: 0,
          }
        : await continueTask(...args);
      simulatedOwner = {
        runId: started.id,
        ownerRevision: `sha256:${"2".repeat(64)}`,
      };
      return started;
    },
    attachRun: attachRun ?? (async (_workspaceId, input) => ({
      run: {
        id: input.runId,
        sessionId: input.sessionId,
        status: "running",
        permissionMode: "manual",
        streamSequence: 0,
      },
      events: [],
      streamState: "live",
      hasGap: false,
    })),
    cancelRun: async (_workspaceId, sessionId, runId) => ({
      id: runId,
      sessionId,
      status: "cancel_requested",
      permissionMode: "manual",
      streamSequence: 1,
    }),
    cancelTerminalTask: async (_workspaceId, sessionId, runId, taskId, expectedGeneration) => ({
      commandId: "terminal-cancel-test",
      clientId: "desktop-test",
      sessionId,
      runId,
      terminalTask: {
        taskId,
        executionBackend: "local_pty" as const,
        sandboxProfile: "workspace_write" as const,
        generation: expectedGeneration + 1,
        status: "cancelled",
        readiness: "none",
        totalOutputBytes: 0,
        emittedAtMs: 2,
      },
      replayed: false,
    }),
    pauseTask: async (_workspaceId, sessionId, runId) => ({
      id: runId,
      sessionId,
      status: "paused",
      permissionMode: "manual",
      streamSequence: 2,
    }),
    planDecision: async (_workspaceId, sessionId, planId, _planHash, action) => ({
      commandId: "plan-decision-command-test",
      clientId: "desktop-test",
      sessionId,
      planId,
      planHash: `sha256:${"a".repeat(64)}`,
      action,
      replayed: false,
    }),
    planDetail: async (_workspaceId, _sessionId, planId, expectedPlanHash) => ({
      planId,
      planHash: expectedPlanHash,
      source: "explicit_plan_command",
      summary: "Inspect and update the requested files",
      steps: [{
        stepId: "inspect",
        title: "Inspect the current implementation",
        dependsOn: [],
        targetPaths: ["src/lib.rs"],
        suggestedChecks: [],
        notes: [],
      }],
      targetPaths: ["src/lib.rs"],
      suggestedChecks: [],
      notes: [],
      lineage: { source: {}, createdAtMs: 1 },
    }),
    userInputRequest: async () => {
      throw new Error("no pending user input");
    },
    userInputDecision: async () => {
      throw new Error("no pending user input");
    },
    resolveApproval: async (_workspaceId, sessionId, runId, approval, decision) => ({
      commandId: "approval-command-test",
      clientId: "desktop-test",
      sessionId,
      runId,
      callId: approval.callId,
      approvalRequestId: approval.approvalRequestId,
      expectedStreamSequence: 1,
      decision: decision === "deny"
        ? "denied"
        : decision === "approve_session" ? "approved_for_session" : "approved",
      routeState: "decision_accepted",
      registryRevision: 2,
      replayed: false,
    }),
    verification: async () => {
      throw new Error("no verification projection");
    },
    rerunVerification: async () => {
      throw new Error("no verification projection");
    },
    taskIntegrationReview: async () => {
      throw { code: "task_integration_review_not_found" };
    },
    acceptTaskIntegration: async () => {
      throw new Error("no integration review");
    },
    intentStack: async () => ({
      status: "not_created",
      schemaVersion: 1,
      safeMessage: "No Intent Stack has been created in this session.",
    }),
    previewIntentDrop: async () => {
      throw new Error("no Intent Drop preview");
    },
    executeIntentDrop: async () => {
      throw new Error("no Intent Drop preview");
    },
    subscribeRunEvents: async () => () => undefined,
    subscribeRunApprovalSnapshots: async () => () => undefined,
    subscribeRunStreamStatus: async () => () => undefined,
    subscribeAppearance: async () => () => undefined,
    subscribeUpdate: async () => () => undefined,
    ...remainingOverrides,
  };
}

async function readyComposer(): Promise<HTMLTextAreaElement> {
  const composer = await screen.findByRole("combobox", { name: "Message Sigil" }) as HTMLTextAreaElement;
  await waitFor(() => expect(composer.disabled).toBe(false));
  return composer;
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
    expect(screen.queryByText("<script>alert(1)</script>")).toBeNull();
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

  it("opens streaming reasoning, then collapses it to a bounded preview on completion", async () => {
    const user = userEvent.setup();
    const text = "first reasoning line\nsecond reasoning line\nthird reasoning line\nhidden reasoning line";
    const { rerender, unmount } = render(
      <Message message={{ key: "reasoning", kind: "reasoning", label: "Working", text, status: "streaming" }} />,
    );
    const disclosure = screen.getByText("Working").closest("details") as HTMLDetailsElement;
    expect(disclosure.open).toBe(true);
    expect(screen.getByText("Hide details")).toBeTruthy();
    expect(disclosure.querySelector(".message-disclosure-preview")).toBeNull();

    rerender(
      <Message message={{ key: "reasoning", kind: "reasoning", label: "Reasoning", text }} />,
    );
    await waitFor(() => expect(disclosure.open).toBe(false));
    expect(screen.getByText("Show details")).toBeTruthy();
    const preview = disclosure.querySelector(".message-disclosure-preview");
    expect(preview?.textContent).toContain("third reasoning line");
    expect(preview?.textContent).not.toContain("hidden reasoning line");
    await user.click(screen.getByText("Show details"));
    expect(disclosure.open).toBe(true);
    expect(screen.getByText("Hide details")).toBeTruthy();
    unmount();
  });

  it("shows a user-selected skill as durable message context", () => {
    render(
      <Message
        message={{
          key: "user-skill",
          kind: "user",
          label: "You",
          text: "Research Chang'an",
          skill: { id: "compat-skill-123", name: "唐代城市研究" },
        }}
      />,
    );

    expect(screen.getByLabelText("Used skill: 唐代城市研究")).toBeTruthy();
    expect(screen.getByText("唐代城市研究")).toBeTruthy();
  });

  it("renders read-only bounded tool and diff surfaces", async () => {
    const user = userEvent.setup();
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
    expect(screen.getByText("245 lines of output.")).toBeTruthy();
    expect(screen.getByLabelText("shell output content").textContent).toContain("line 3");
    expect(screen.getByLabelText("shell output content").textContent).not.toContain("line 4");
    await user.click(screen.getByRole("button", { name: "Expand shell output" }));
    expect(screen.getByText("5 output lines omitted from this view.")).toBeTruthy();
    expect(screen.getByLabelText("shell output content").textContent).toContain("line 240");
    expect(screen.queryByText("duration not recorded")).toBeNull();
    expect(screen.queryByText("risk not classified")).toBeNull();
  });

  it("retrieves saved tool output through bounded typed selectors", async () => {
    const user = userEvent.setup();
    const nextSelector = { kind: "byte_slice" as const, offset: 16, limit: 16 };
    const readArtifact = vi.fn(async (selector: ToolArtifactSelector): Promise<ToolArtifactPage> => ({
      schemaVersion: 1 as const,
      requestScope: "session-1",
      artifactRef: `ta1_${"a".repeat(32)}`,
      selector,
      body: selector.kind === "search_literal" ? "matching context" : "saved page",
      bodyEncoding: "utf8" as const,
      returnedBytes: selector.kind === "search_literal" ? 16 : 10,
      pageSha256: `sha256:${"b".repeat(64)}`,
      artifactSha256: `sha256:${"c".repeat(64)}`,
      eof: selector.kind === "search_literal",
      matchCount: selector.kind === "search_literal" ? 1 : 0,
      nextSelector: selector.kind === "search_literal" ? undefined : nextSelector,
    }));
    render(
      <ToolCard
        tool={{
          key: "saved-tool",
          toolName: "shell",
          text: "bounded preview",
          status: "succeeded",
          artifactRef: `ta1_${"a".repeat(32)}`,
          artifactAvailability: "available",
          artifactHasMore: true,
          artifactPersistedBytes: 32,
        }}
        onReadArtifact={readArtifact}
      />,
    );

    expect(screen.getByText("32 saved bytes are available in bounded pages.")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "View saved output" }));
    expect((await screen.findByLabelText("shell saved output page")).textContent).toContain("saved page");
    expect(readArtifact).toHaveBeenNthCalledWith(1, {
      kind: "byte_slice",
      offset: 0,
      limit: 16_384,
    });

    await user.click(screen.getByRole("button", { name: "Next page" }));
    expect(readArtifact).toHaveBeenNthCalledWith(2, nextSelector);

    await user.type(screen.getByRole("textbox", { name: "Search saved tool output" }), "needle");
    await user.click(screen.getByRole("button", { name: "Search" }));
    expect(await screen.findByText("matching context")).toBeTruthy();
    expect(readArtifact).toHaveBeenNthCalledWith(3, {
      kind: "search_literal",
      query: "needle",
      startOffset: 0,
      maxMatches: 20,
      contextLines: 2,
    });
  });

  it("fully shows short streaming tool output", () => {
    render(
      <ToolCard tool={{ key: "short-streaming-tool", toolName: "bash", text: "still working", status: "running" }} />,
    );

    expect(screen.getByText("still working").tagName).toBe("P");
    expect(screen.queryByRole("button", { name: /bash output/i })).toBeNull();
  });

  it("fully expands long streaming tool output and contracts it after completion", async () => {
    const output = Array.from({ length: 8 }, (_, index) => `stream line ${index + 1}`).join("\n");
    const { rerender } = render(
      <ToolCard tool={{ key: "streaming-tool", toolName: "bash", text: output, status: "running" }} />,
    );

    expect(screen.getByRole("button", { name: "Collapse bash output" })).toBeTruthy();
    expect(screen.getByLabelText("bash output content").textContent).toContain("stream line 8");

    rerender(
      <ToolCard tool={{ key: "streaming-tool", toolName: "bash", text: output, status: "completed" }} />,
    );
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Expand bash output" })).toBeTruthy();
    });
    expect(screen.getByLabelText("bash output content").textContent).toContain("stream line 3");
    expect(screen.getByLabelText("bash output content").textContent).not.toContain("stream line 4");
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
      summary: "1 line read.",
      detailKind: "output",
      detailText: "pub fn ready() -> bool { true }",
      detailLanguage: "rust",
    });

    render(<ToolCard tool={tool} />);
    expect(screen.getByText("path=src/lib.rs")).toBeTruthy();
    expect(screen.getByText("Result")).toBeTruthy();
    expect(screen.getByText("1 line read.")).toBeTruthy();
    expect(screen.getByText("Output")).toBeTruthy();
    expect(screen.getByText("Rust · 1 line")).toBeTruthy();
    expect(screen.getByLabelText("read_file output content").querySelector(".hljs-keyword")?.textContent).toBe("pub");
    expect(screen.queryByRole("button", { name: /show output|show all/i })).toBeNull();
    expect(screen.queryByText("Tool activity was recorded.")).toBeNull();
  });

  it("treats empty structured tool payloads as compact semantic results", () => {
    const text = JSON.stringify({
      content: "[]",
      meta: {
        details: {
          call: { summary: "path=crates/sigil-kernel/src pattern=unwrap" },
        },
      },
      status: "ok",
    });
    const tool = { key: "empty-grep", toolName: "grep", text };

    expect(presentTool(tool)).toMatchObject({
      displayName: "Grep",
      status: "Ok",
      tone: "success",
      summary: "No matches found.",
      input: "path=crates/sigil-kernel/src pattern=unwrap",
    });
    expect(presentTool(tool).detailKind).toBeUndefined();

    render(<ToolCard tool={tool} />);
    expect(screen.getByText("No matches found.")).toBeTruthy();
    expect(screen.getByLabelText("grep input")).toBeTruthy();
    expect(screen.queryByText("Command")).toBeNull();
    expect(screen.queryByText("View output")).toBeNull();
    expect(screen.queryByText("[]")).toBeNull();
  });

  it("summarizes non-empty JSON collections and reveals structured output on demand", async () => {
    const user = userEvent.setup();
    const content = JSON.stringify([
      { path: "src/one.rs", line: 4 },
      { path: "src/two.rs", line: 9 },
    ], null, 2);
    const text = JSON.stringify({
      content,
      meta: {
        details: {
          call: { summary: "path=src pattern=todo" },
        },
      },
      status: "ok",
    });
    const tool = { key: "grep-results", toolName: "grep", text };

    expect(presentTool(tool)).toMatchObject({
      summary: "2 matches.",
      detailKind: "output",
      detailLanguage: "json",
    });

    render(<ToolCard tool={tool} />);
    expect(screen.getByText("Result")).toBeTruthy();
    expect(screen.getByText("2 matches.")).toBeTruthy();
    expect(screen.getByText("Json · 10 lines")).toBeTruthy();
    expect(screen.getByLabelText("grep output content").textContent).toContain('"path"');
    expect(screen.getByLabelText("grep output content").textContent).not.toContain("src/two.rs");
    await user.click(screen.getByRole("button", { name: "Expand grep output" }));
    expect(screen.getByLabelText("grep output content")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Collapse grep output" })).toBeTruthy();
  });

  it("keeps raw JSON objects visible instead of mistaking them for tool envelopes", () => {
    const tool = {
      key: "raw-json-result",
      toolName: "inspect",
      text: JSON.stringify({ path: "src/main.rs", lines: 42 }, null, 2),
      status: "ok",
    };

    expect(presentTool(tool)).toMatchObject({
      summary: "2 fields of structured output.",
      detailKind: "output",
      detailLanguage: "json",
    });

    render(<ToolCard tool={tool} />);
    expect(screen.getByText("2 fields of structured output.")).toBeTruthy();
    expect(screen.getByText("Json · 4 lines")).toBeTruthy();
  });

  it("labels scalar shell output as a result instead of leaving a bare number", () => {
    const tool = {
      key: "shell-count",
      toolName: "bash",
      text: JSON.stringify({
        content: "1551",
        meta: {
          details: {
            call: { command: "grep -rn 'pub fn' crates/sigil-kernel/src | wc -l" },
          },
        },
        status: "ok",
      }),
    };

    expect(presentTool(tool)).toMatchObject({
      summary: "1551",
      input: "grep -rn 'pub fn' crates/sigil-kernel/src | wc -l",
    });
    expect(presentTool(tool).detailKind).toBeUndefined();

    render(<ToolCard tool={tool} />);
    expect(screen.getByText("Result")).toBeTruthy();
    expect(screen.getByText("1551").tagName).toBe("P");
    expect(screen.queryByText("View output")).toBeNull();
    expect(screen.queryByRole("button", { name: /show output|show all/i })).toBeNull();
  });
});

describe("desktop workspace and history shell", () => {
  it("persists a theme switch and preserves the active conversation draft", async () => {
    const user = userEvent.setup();
    const setAppearance = vi.fn(async () => ({
      preference: "solarized_light" as const,
      resolvedTheme: "solarized_light" as const,
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
    const composer = await readyComposer();
    await user.type(composer, "Keep this draft");
    await user.click(screen.getByRole("button", { name: "Open settings" }));
    expect(await screen.findByText(/Use environment variable · api\.deepseek\.com/)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Solarized Light" }));

    await waitFor(() => expect(document.documentElement.dataset.theme).toBe("solarized_light"));
    expect(document.documentElement.dataset.colorScheme).toBe("light");
    expect(setAppearance).toHaveBeenCalledWith("solarized_light");
    await user.click(screen.getByLabelText("Sigil desktop home"));
    expect((screen.getByLabelText("Message Sigil") as HTMLTextAreaElement).value).toBe("Keep this draft");
  });

  it("opens explicit provider setup from Settings without an existing conversation", async () => {
    const user = userEvent.setup();
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
    })} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "Open settings" }));
    expect(await screen.findByText(/Use environment variable · api\.deepseek\.com/)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Add connection" }));

    expect(await screen.findByRole("heading", { name: "Add connection" })).toBeTruthy();
    expect(screen.getByRole("button", { name: /DeepSeek/ })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("heading", { name: "Add connection" })).toBeNull();
  });

  it("keeps the proven theme on save failure and exposes a scoped retry", async () => {
    const user = userEvent.setup();
    let attempts = 0;
    const bridge = bridgeWith({
      setAppearance: async () => {
        attempts += 1;
        if (attempts === 1) throw new Error("store unavailable");
        return { preference: "solarized_light", resolvedTheme: "solarized_light" };
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByRole("heading", { name: "Open a workspace" });
    await user.click(screen.getByRole("button", { name: "Open settings" }));
    await user.click(screen.getByRole("button", { name: "Solarized Light" }));
    expect((await screen.findByRole("alert")).textContent).toContain("previous appearance is still active");
    expect(document.documentElement.dataset.theme).toBe("sigil_dark");

    await user.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(document.documentElement.dataset.theme).toBe("solarized_light"));
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
    act(() => listener?.({ preference: "system", resolvedTheme: "sigil_light" }));
    expect(document.documentElement.dataset.themePreference).toBe("system");
    expect(document.documentElement.dataset.theme).toBe("sigil_light");
    expect(document.documentElement.dataset.colorScheme).toBe("light");
    expect(screen.getByRole("button", { name: "System theme" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("tolerates stale native appearance cleanup after a WebView reload", async () => {
    let resolveSubscription: (unsubscribe: () => void) => void = () => undefined;
    const subscribeAppearance = vi.fn(
      () =>
        new Promise<() => void>((resolve) => {
          resolveSubscription = resolve;
        }),
    );
    const view = render(<App bridge={bridgeWith({ subscribeAppearance })} />);

    await screen.findByRole("heading", { name: "Open a workspace" });
    view.unmount();
    resolveSubscription(() => {
      throw new Error("listener registry was already reset");
    });
    await act(async () => Promise.resolve());

    expect(subscribeAppearance).toHaveBeenCalledOnce();
  });

  it("uses the topbar theme control as a bounded shortcut for named palettes", async () => {
    const user = userEvent.setup();
    const setAppearance = vi.fn(async () => ({
      preference: "system" as const,
      resolvedTheme: "sigil_dark" as const,
    }));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [],
        recentWorkspaces: [],
        appearance: { preference: "nord", resolvedTheme: "nord" },
      }),
      setAppearance,
    })} />);

    await screen.findByRole("heading", { name: "Open a workspace" });
    await user.click(screen.getByRole("button", { name: "Nord. Switch to system theme" }));
    await waitFor(() => expect(setAppearance).toHaveBeenCalledWith("system"));
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

  it("shows the first-run path before a workspace is selected", async () => {
    render(<App bridge={bridgeWith()} />);

    expect(await screen.findByRole("heading", { name: "Open a workspace" })).toBeTruthy();
    expect(screen.getByRole("list", { name: "Getting started" }).textContent).toContain(
      "Open a project",
    );
    expect(screen.getByRole("list", { name: "Getting started" }).textContent).toContain(
      "Connect a provider and choose a model",
    );
    expect(screen.getByRole("list", { name: "Getting started" }).textContent).toContain(
      "Start the first conversation",
    );
  });

  it("blocks the first conversation on explicit provider setup and saves the chosen route", async () => {
    const user = userEvent.setup();
    const providerSetupCatalog = vi.fn(async () => ({
      connectionId: "deepseek-1",
      providerLabel: "DeepSeek",
      state: "remote",
      models: [{
        modelId: "deepseek-v4-flash",
        displayName: "DeepSeek V4 Flash",
        availability: "available" as const,
        recommended: true,
        provenance: "remote" as const,
        contextWindowTokens: 1_000_000,
      }],
      suggestedModel: "deepseek-v4-flash",
      manualEntryAllowed: false,
      orchestrationRollout: {
        status: "route_not_qualified",
        routingPolicy: "auto",
        multiAgentMode: "explicit_request_only",
        reason: "this release has no qualified orchestration route manifest",
      },
    }));
    const saveProviderSetup = vi.fn(async () => ({
      defaultModel: { connectionId: "deepseek-1", modelId: "deepseek-v4-flash" },
      inventory: {
        configMode: "v2" as const,
        defaultModel: { connectionId: "deepseek-1", modelId: "deepseek-v4-flash" },
        connections: [{
          id: "deepseek-1",
          label: "DeepSeek 1",
          providerLabel: "DeepSeek",
          protocolLabel: "DeepSeek",
          endpointDisplay: "api.deepseek.com",
          credentialSource: "stored" as const,
          readiness: "ready" as const,
          modelContextWindows: { "deepseek-v4-flash": 262_144 },
          defaultModel: { connectionId: "deepseek-1", modelId: "deepseek-v4-flash" },
        }],
        issues: [],
      },
      saveWarning: false,
    }));
    const createSession = vi.fn();
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      providerConnections: async () => ({
        configMode: "v2",
        connections: [],
        issues: [],
      }),
      providerSetupCatalog,
      saveProviderSetup,
      createSession,
    })} />);

    expect(await screen.findByRole("heading", { name: "Connect a model provider" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "New conversation" }).hasAttribute("disabled"))
      .toBe(true);
    await user.click(screen.getByRole("button", { name: /DeepSeek/ }));
    await user.type(screen.getByLabelText("API key"), "secret-canary");
    await user.click(screen.getByRole("button", { name: "Continue to models" }));
    expect(await screen.findByRole("radio", { name: /DeepSeek V4 Flash/ })).toBeTruthy();
    expect(screen.getByText("Automatic routing: auto")).toBeTruthy();
    expect(screen.getByText("Direct task execution: review first fallback")).toBeTruthy();
    expect(screen.getByText("this release has no qualified orchestration route manifest")).toBeTruthy();
    expect(screen.getByLabelText("Context window").getAttribute("placeholder")).toBe("1000000");
    await user.type(screen.getByLabelText("Context window"), "262144");
    await user.click(screen.getByRole("button", { name: "Save and continue" }));

    await waitFor(() => expect(saveProviderSetup).toHaveBeenCalledWith(
      workspace.id,
      expect.objectContaining({
        template: "deep_seek",
        credentialSource: "secure_store",
        apiKey: "secret-canary",
        modelId: "deepseek-v4-flash",
        contextWindowTokens: 262144,
      }),
    ));
    expect(await screen.findByRole("heading", { name: "Select a conversation" })).toBeTruthy();
    expect(createSession).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Open settings" }));
    expect(await screen.findByRole("heading", { name: "Settings" })).toBeTruthy();
    expect(screen.getByLabelText("Context window").getAttribute("value")).toBe("262144");
  });

  it("ignores a completed first-run provider setup after switching workspaces", async () => {
    const user = userEvent.setup();
    const betaWorkspace: WorkspaceSummary = {
      id: "workspace-beta-provider-setup",
      displayName: "beta",
      serverVersion: "0.0.1-alpha.5",
      state: "ready",
    };
    const betaInventory = {
      configMode: "v2" as const,
      defaultModel: { connectionId: "beta", modelId: "beta-model" },
      connections: [{
        id: "beta",
        label: "Beta Provider",
        providerLabel: "OpenAI",
        protocolLabel: "Responses",
        endpointDisplay: "beta.example",
        credentialSource: "environment" as const,
        readiness: "ready" as const,
        defaultModel: { connectionId: "beta", modelId: "beta-model" },
      }],
      issues: [],
    };
    const alphaSavedInventory = {
      configMode: "v2" as const,
      defaultModel: { connectionId: "deepseek-1", modelId: "deepseek-v4-flash" },
      connections: [{
        id: "deepseek-1",
        label: "Alpha Provider",
        providerLabel: "DeepSeek",
        protocolLabel: "DeepSeek",
        endpointDisplay: "api.deepseek.com",
        credentialSource: "stored" as const,
        readiness: "ready" as const,
        defaultModel: { connectionId: "deepseek-1", modelId: "deepseek-v4-flash" },
      }],
      issues: [],
    };
    let resolveSave: ((result: Awaited<
      ReturnType<DesktopBridge["saveProviderSetup"]>
    >) => void) | undefined;
    const saveProviderSetup = vi.fn(() =>
      new Promise<Awaited<ReturnType<DesktopBridge["saveProviderSetup"]>>>((resolve) => {
        resolveSave = resolve;
      })
    );
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace, betaWorkspace],
        recentWorkspaces: [],
      }),
      providerConnections: async (workspaceId) =>
        workspaceId === workspace.id
          ? { configMode: "v2", connections: [], issues: [] }
          : betaInventory,
      providerSetupCatalog: async () => ({
        connectionId: "deepseek-1",
        providerLabel: "DeepSeek",
        state: "remote",
        models: [{
          modelId: "deepseek-v4-flash",
          displayName: "DeepSeek V4 Flash",
          availability: "available",
          recommended: true,
          provenance: "remote",
        }],
        suggestedModel: "deepseek-v4-flash",
        manualEntryAllowed: false,
      }),
      saveProviderSetup,
    })} />);

    await screen.findByRole("heading", { name: "Connect a model provider" });
    await user.click(screen.getByRole("button", { name: /DeepSeek/ }));
    await user.type(screen.getByLabelText("API key"), "alpha-secret");
    await user.click(screen.getByRole("button", { name: "Continue to models" }));
    await screen.findByRole("radio", { name: /DeepSeek V4 Flash/ });
    await user.click(screen.getByRole("button", { name: "Save and continue" }));
    await waitFor(() => expect(saveProviderSetup).toHaveBeenCalledTimes(1));

    await user.click(screen.getByRole("button", { name: "Switch workspace: sigil" }));
    await user.click(screen.getByRole("button", { name: "beta" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Switch workspace: beta" })).toBeTruthy()
    );
    await user.click(screen.getByRole("button", { name: "Open settings" }));
    expect(await screen.findByText("Beta Provider")).toBeTruthy();

    await act(async () => resolveSave?.({
      defaultModel: alphaSavedInventory.defaultModel,
      inventory: alphaSavedInventory,
      saveWarning: false,
    }));

    expect(screen.getByText("Beta Provider")).toBeTruthy();
    expect(screen.queryByText("Alpha Provider")).toBeNull();
    expect(screen.queryByText("Provider connection saved")).toBeNull();
  });

  it("suppresses a stale provider reload failure after switching workspaces", async () => {
    const user = userEvent.setup();
    const betaWorkspace: WorkspaceSummary = {
      id: "workspace-beta-provider-reload",
      displayName: "beta",
      serverVersion: "0.0.1-alpha.5",
      state: "ready",
    };
    const betaInventory = {
      configMode: "v2" as const,
      defaultModel: { connectionId: "beta", modelId: "beta-model" },
      connections: [{
        id: "beta",
        label: "Beta Provider",
        providerLabel: "OpenAI",
        protocolLabel: "Responses",
        endpointDisplay: "beta.example",
        credentialSource: "environment" as const,
        readiness: "ready" as const,
        defaultModel: { connectionId: "beta", modelId: "beta-model" },
      }],
      issues: [],
    };
    let rejectReload: ((error: Error) => void) | undefined;
    let alphaLoads = 0;
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace, betaWorkspace],
        recentWorkspaces: [],
      }),
      providerConnections: async (workspaceId) => {
        if (workspaceId === betaWorkspace.id) return betaInventory;
        alphaLoads += 1;
        if (alphaLoads === 1) {
          return {
            configMode: "invalid" as const,
            connections: [],
            issues: [{
              code: "invalid_provider_schema",
              message: "invalid provider schema",
            }],
          };
        }
        return new Promise<never>((_resolve, reject) => {
          rejectReload = reject;
        });
      },
    })} />);

    await screen.findByRole("heading", { name: "Provider configuration is invalid" });
    await user.click(screen.getAllByRole("button", { name: "Open settings" }).at(-1)!);
    await user.click(await screen.findByRole("button", { name: "Recheck configuration" }));
    await user.click(screen.getByRole("button", { name: "Switch workspace: sigil" }));
    await user.click(screen.getByRole("button", { name: "beta" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Switch workspace: beta" })).toBeTruthy()
    );
    await user.click(screen.getByRole("button", { name: "Open settings" }));
    expect(await screen.findByText("Beta Provider")).toBeTruthy();

    await act(async () => rejectReload?.(new Error("alpha reload failed")));

    expect(screen.getByText("Beta Provider")).toBeTruthy();
    expect(screen.queryByText("Provider settings are unavailable")).toBeNull();
  });

  it("repairs invalid provider configuration from Settings with explicit replacement intent", async () => {
    const user = userEvent.setup();
    const invalidInventory = {
      configMode: "invalid" as const,
      connections: [],
      issues: [{
        code: "config_invalid_current_schema",
        message: "invalid current configuration",
      }],
    };
    const repairedInventory = {
      configMode: "v2" as const,
      defaultModel: { connectionId: "deepseek-1", modelId: "deepseek-v4-flash" },
      connections: [{
        id: "deepseek-1",
        label: "DeepSeek 1",
        providerLabel: "DeepSeek",
        protocolLabel: "DeepSeek",
        endpointDisplay: "api.deepseek.com",
        credentialSource: "stored" as const,
        readiness: "ready" as const,
        defaultModel: { connectionId: "deepseek-1", modelId: "deepseek-v4-flash" },
      }],
      issues: [],
    };
    const providerSetupCatalog = vi.fn(async () => ({
      connectionId: "deepseek-1",
      providerLabel: "DeepSeek",
      state: "remote",
      models: [{
        modelId: "deepseek-v4-flash",
        displayName: "DeepSeek V4 Flash",
        availability: "available" as const,
        recommended: true,
        provenance: "remote" as const,
      }],
      suggestedModel: "deepseek-v4-flash",
      manualEntryAllowed: false,
    }));
    const saveProviderSetup = vi.fn(async () => ({
      defaultModel: repairedInventory.defaultModel,
      inventory: repairedInventory,
      saveWarning: false,
    }));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      providerConnections: async () => invalidInventory,
      providerSetupCatalog,
      saveProviderSetup,
    })} />);

    await screen.findByRole("heading", { name: "Provider configuration is invalid" });
    await user.click(screen.getAllByRole("button", { name: "Open settings" }).at(-1)!);
    await user.click(await screen.findByRole("button", { name: "Replace invalid configuration" }));
    expect(
      await screen.findByRole("heading", { name: "Replace invalid configuration" }),
    ).toBeTruthy();
    await user.click(screen.getByRole("button", { name: /DeepSeek/ }));
    await user.type(screen.getByLabelText("API key"), "replacement-secret");
    await user.click(screen.getByRole("button", { name: "Continue to models" }));
    await screen.findByRole("radio", { name: /DeepSeek V4 Flash/ });
    await user.click(screen.getByRole("button", { name: "Replace configuration and continue" }));

    await waitFor(() => expect(providerSetupCatalog).toHaveBeenCalledWith(
      workspace.id,
      expect.objectContaining({ replaceInvalidConfig: true }),
    ));
    await waitFor(() => expect(saveProviderSetup).toHaveBeenCalledWith(
      workspace.id,
      expect.objectContaining({
        replaceInvalidConfig: true,
        apiKey: "replacement-secret",
      }),
    ));
    expect(await screen.findByText("DeepSeek 1")).toBeTruthy();
  });

  it("shows the Sigil environment variable required by the selected provider", async () => {
    const user = userEvent.setup();
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      providerConnections: async () => ({
        configMode: "v2",
        connections: [],
        issues: [],
      }),
    })} />);

    await user.click(await screen.findByRole("button", { name: /^OpenAI(?!-compatible)/ }));
    await user.selectOptions(screen.getByLabelText("Authentication"), "environment");

    expect(screen.getByText(/SIGIL_OPENAI_RESPONSES_API_KEY/)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Change" }));
    await user.click(screen.getByRole("button", { name: /OpenAI-compatible/ }));
    await user.selectOptions(screen.getByLabelText("API protocol"), "responses");
    await user.selectOptions(screen.getByLabelText("Authentication"), "environment");

    expect(screen.getByText(/SIGIL_OPENAI_RESPONSES_API_KEY/)).toBeTruthy();
  });

  it("explains rejected catalog credentials while keeping explicit model setup available", async () => {
    const user = userEvent.setup();
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      providerConnections: async () => ({
        configMode: "v2",
        connections: [],
        issues: [],
      }),
      providerSetupCatalog: async () => ({
        connectionId: "deepseek-1",
        providerLabel: "DeepSeek",
        state: "auth_rejected",
        models: [],
        manualEntryAllowed: false,
      }),
    })} />);

    await user.click(await screen.findByRole("button", { name: /DeepSeek/ }));
    await user.type(screen.getByLabelText("API key"), "rejected-secret-canary");
    await user.click(screen.getByRole("button", { name: "Continue to models" }));

    expect((await screen.findByRole("alert")).textContent).toContain(
      "The model list rejected this API key. You can still enter an exact model ID and continue.",
    );
    expect(screen.getByRole("radio", { name: /Enter a model ID/ })).toBeTruthy();
    await user.type(screen.getByLabelText("Model ID"), "explicit-model");
    expect((screen.getByRole("button", { name: "Save and continue" }) as HTMLButtonElement).disabled)
      .toBe(false);
  });

  it("keeps a stale catalog visible and allows save during optional refresh", async () => {
    const user = userEvent.setup();
    let now = 1_784_505_600_000;
    vi.spyOn(Date, "now").mockImplementation(() => now);
    let resolveRefresh: ((catalog: ProviderSetupCatalog) => void) | undefined;
    const catalog: ProviderSetupCatalog = {
      connectionId: "local-stale-1",
      providerLabel: "OpenAI-compatible",
      state: "remote",
      models: [{
        modelId: "local-stale-coder",
        displayName: "Local Stale Coder",
        availability: "available",
        recommended: true,
        provenance: "remote",
      }],
      suggestedModel: "local-stale-coder",
      manualEntryAllowed: true,
    };
    const providerSetupCatalog = vi.fn()
      .mockResolvedValueOnce(catalog)
      .mockImplementationOnce(() => new Promise<ProviderSetupCatalog>((resolve) => {
        resolveRefresh = resolve;
      }));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      providerConnections: async () => ({
        configMode: "v2",
        connections: [],
        issues: [],
      }),
      providerSetupCatalog,
    })} />);

    await user.click(await screen.findByRole("button", { name: /OpenAI-compatible/ }));
    await user.type(screen.getByLabelText("API endpoint"), "http://127.0.0.1:11438/v1");
    await user.selectOptions(screen.getByLabelText("Authentication"), "none");
    await user.click(screen.getByRole("button", { name: "Continue to models" }));
    expect(await screen.findByRole("radio", { name: /Local Stale Coder/ })).toBeTruthy();

    now += 10 * 60 * 1_000 + 1;
    await user.click(screen.getByRole("button", { name: "Change connection" }));
    await user.click(screen.getByRole("button", { name: "Continue to models" }));

    expect(await screen.findByText(/Stale cached catalog/)).toBeTruthy();
    expect(screen.getByRole("radio", { name: /Local Stale Coder/ })).toBeTruthy();
    expect((screen.getByRole("button", { name: "Save and continue" }) as HTMLButtonElement).disabled)
      .toBe(false);
    expect(await screen.findByRole("button", { name: "Refreshing models…" })).toBeTruthy();
    expect(providerSetupCatalog).toHaveBeenCalledTimes(2);

    await user.click(screen.getByRole("button", { name: "Change connection" }));
    await act(async () => {
      resolveRefresh?.(catalog);
      await Promise.resolve();
    });

    expect(screen.getByLabelText("API endpoint")).toBeTruthy();
    expect(screen.queryByRole("radio", { name: /Local Stale Coder/ })).toBeNull();
    expect(screen.queryByRole("button", { name: "Save and continue" })).toBeNull();
  });

  it("drops an in-flight catalog and staged key when the provider changes", async () => {
    const user = userEvent.setup();
    let resolveCatalog: ((catalog: ProviderSetupCatalog) => void) | undefined;
    const providerSetupCatalog = vi.fn(() => new Promise<ProviderSetupCatalog>((resolve) => {
      resolveCatalog = resolve;
    }));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      providerConnections: async () => ({
        configMode: "v2",
        connections: [],
        issues: [],
      }),
      providerSetupCatalog,
    })} />);

    await user.click(await screen.findByRole("button", { name: /DeepSeek/ }));
    await user.type(screen.getByLabelText("API key"), "deepseek-secret-canary");
    await user.click(screen.getByRole("button", { name: "Continue to models" }));
    await user.click(screen.getByRole("button", { name: "Change" }));
    await user.click(screen.getByRole("button", { name: /^OpenAI(?!-compatible)/ }));

    expect((screen.getByLabelText("API key") as HTMLInputElement).value).toBe("");
    await act(async () => {
      resolveCatalog?.({
        connectionId: "deepseek-1",
        providerLabel: "DeepSeek",
        state: "remote",
        models: [{
          modelId: "deepseek-v4-flash",
          displayName: "DeepSeek V4 Flash",
          availability: "available",
          recommended: true,
          provenance: "remote",
        }],
        suggestedModel: "deepseek-v4-flash",
        manualEntryAllowed: false,
      });
    });

    expect(screen.queryByRole("radio", { name: /DeepSeek V4 Flash/ })).toBeNull();
    expect(providerSetupCatalog).toHaveBeenCalledTimes(1);
  });

  it("persists a provider-validated default model for newly created desktop conversations", async () => {
    const user = userEvent.setup();
    let sequence = 0;
    const createSession = vi.fn(async () => ({
      id: `http-session-${++sequence}`,
      label: "New conversation",
      runCount: 0,
    }));
    const saveProviderDefaultModel = vi.fn(async (_workspaceId: string, modelRef: {
      connectionId: string;
      modelId: string;
    }) => ({
      defaultModel: modelRef,
      inventory: {
        configMode: "v2" as const,
        defaultModel: modelRef,
        connections: [{
          id: "deepseek-default",
          label: "DeepSeek",
          providerLabel: "DeepSeek",
          protocolLabel: "DeepSeek",
          endpointDisplay: "api.deepseek.com",
          credentialSource: "environment" as const,
          readiness: "ready" as const,
          defaultModel: modelRef,
        }],
        issues: [],
      },
      saveWarning: false,
    }));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      createSession,
      saveProviderDefaultModel,
    })} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    await screen.findByRole("combobox", { name: "Model" });
    await user.click(screen.getByRole("button", { name: "Open settings" }));
    expect(await screen.findByText(
      "New conversation default: DeepSeek V4 Flash · deepseek-v4-flash",
    )).toBeTruthy();
    expect(screen.getByText(/Connection DeepSeek · Provider DeepSeek/)).toBeTruthy();
    await user.selectOptions(
      await screen.findByRole("combobox", { name: "Default model for new conversations" }),
      "deepseek-default/deepseek-v4-pro",
    );
    await waitFor(() => expect(saveProviderDefaultModel).toHaveBeenCalledWith(
      workspace.id,
      { connectionId: "deepseek-default", modelId: "deepseek-v4-pro" },
    ));
    const contextWindowInput = screen.getByLabelText("Context window");
    await user.type(contextWindowInput, "524288");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(saveProviderDefaultModel).toHaveBeenLastCalledWith(
      workspace.id,
      { connectionId: "deepseek-default", modelId: "deepseek-v4-pro" },
      524288,
    ));
    await user.clear(contextWindowInput);
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(saveProviderDefaultModel).toHaveBeenLastCalledWith(
      workspace.id,
      { connectionId: "deepseek-default", modelId: "deepseek-v4-pro" },
      undefined,
    ));
    await user.click(screen.getByRole("button", { name: "Back to conversations" }));
    await user.click(screen.getByRole("button", { name: "New conversation" }));

    await waitFor(() => expect(createSession).toHaveBeenCalledTimes(2));
    expect(createSession.mock.calls[1]).toEqual([
      workspace.id,
      "New conversation",
      {
        connectionId: "deepseek-default",
        modelId: "deepseek-v4-pro",
      },
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

  it("restores the last selected conversation after the workspace is ready", async () => {
    writeLastSession(workspace.id, {
      sessionRef: "last-session.jsonl",
      sessionId: "durable-last-session",
      label: "Last selected conversation",
    });
    const openSession = vi.fn(async () => ({
      id: "http-restored-session",
      label: "Last selected conversation",
      runCount: 2,
    }));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      openSession,
    })} />);

    expect(await screen.findByRole("heading", { name: "Last selected conversation" })).toBeTruthy();
    expect(openSession).toHaveBeenCalledWith(workspace.id, {
      sessionRef: "last-session.jsonl",
      sessionId: "durable-last-session",
      label: "Last selected conversation",
    });
  });

  it("falls back to the conversation list when the remembered session is stale", async () => {
    writeLastSession(workspace.id, {
      sessionRef: "missing-session.jsonl",
      sessionId: "durable-missing-session",
      label: "Missing conversation",
    });
    const openSession = vi.fn(async () => Promise.reject(new Error("missing")));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      openSession,
    })} />);

    expect(await screen.findByRole("heading", { name: "Select a conversation" })).toBeTruthy();
    await waitFor(() => expect(openSession).toHaveBeenCalledTimes(1));
    expect(readLastSession(workspace.id)).toBeUndefined();
  });

  it("retries the exact recent workspace and exposes only bounded safe recovery details", async () => {
    const user = userEvent.setup();
    const nativeMessage = `${"The desktop runtime is out of sync. ".repeat(30)}Restart the development app.`;
    const openRecentWorkspace = vi.fn()
      .mockRejectedValueOnce({
        code: "workspace_server_unavailable",
        message: nativeMessage,
        path: "/Users/private/project",
        token: "secret-bearer",
      })
      .mockResolvedValueOnce(workspace);
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

    expect(await screen.findByRole("heading", { name: "Workspace needs attention" })).toBeTruthy();
    const details = screen.getByText("Show details").closest("details") as HTMLDetailsElement;
    expect(details.open).toBe(false);
    await user.click(screen.getByText("Show details"));
    expect(details.open).toBe(true);
    const alert = screen.getByRole("alert");
    expect(alert.textContent).toContain("workspace_server_unavailable");
    expect(alert.textContent).toContain(nativeMessage.slice(0, 512));
    expect(alert.textContent).not.toContain("Restart the development app.");
    expect(alert.textContent).not.toContain("/Users/private/project");
    expect(alert.textContent).not.toContain("secret-bearer");
    expect(document.querySelector(".statusbar")).toBeNull();

    await user.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("heading", { name: "Select a conversation" })).toBeTruthy();
    expect(openRecentWorkspace).toHaveBeenNthCalledWith(1, workspace.id);
    expect(openRecentWorkspace).toHaveBeenNthCalledWith(2, workspace.id);
  });

  it("does not infer retry for an incompatible workspace runtime", async () => {
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
        message: "The desktop runtime is out of sync.",
        recoveryActions: ["open_another_workspace", "show_details"],
      }),
    });
    render(<App bridge={bridge} />);

    expect(await screen.findByRole("heading", { name: "Workspace needs attention" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    expect(screen.getByRole("button", { name: "Open another workspace" })).toBeTruthy();
    expect(screen.getByText("Show details")).toBeTruthy();
  });

  it("keeps workspace recovery available when choosing another workspace is cancelled", async () => {
    const user = userEvent.setup();
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [],
        recentWorkspaces: [{ id: workspace.id, displayName: "sigil", isOpen: false }],
      }),
      openRecentWorkspace: async () => Promise.reject({
        code: "recent_workspace_unknown",
        message: "The recent workspace is no longer available.",
      }),
      pickWorkspace: async () => ({ cancelled: true }),
    });
    render(<App bridge={bridge} />);

    expect(await screen.findByRole("heading", { name: "Workspace needs attention" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Open another workspace" }));
    expect(await screen.findByRole("heading", { name: "Workspace needs attention" })).toBeTruthy();
    expect(screen.getByText("The recent workspace is no longer available.")).toBeTruthy();
  });

  it("replaces stale recent-workspace recovery when the workspace picker fails", async () => {
    const user = userEvent.setup();
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [],
        recentWorkspaces: [{ id: workspace.id, displayName: "sigil", isOpen: false }],
      }),
      openRecentWorkspace: async () => Promise.reject({
        code: "recent_workspace_unknown",
        message: "The recent workspace is no longer available.",
        recoveryActions: ["open_another_workspace", "show_details"],
      }),
      pickWorkspace: async () => Promise.reject({
        code: "workspace_server_start_failed",
        message: "The selected workspace runtime could not start.",
        recoveryActions: ["retry_current", "show_details"],
      }),
    });
    render(<App bridge={bridge} />);

    expect(await screen.findByText("The recent workspace is no longer available.")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Open another workspace" }));

    expect(await screen.findByText("The selected workspace runtime could not start.")).toBeTruthy();
    expect(screen.queryByText("The recent workspace is no longer available.")).toBeNull();
    expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Open another workspace" })).toBeNull();
  });

  it("recovers from bootstrap failure without duplicating the failure in the footer", async () => {
    const user = userEvent.setup();
    const bootstrap = vi.fn()
      .mockRejectedValueOnce({ code: "desktop_bootstrap_failed", message: "The local service did not answer.", path: "/private" })
      .mockResolvedValueOnce({ protocolVersion: 2, workspaces: [], recentWorkspaces: [] });
    render(<App bridge={bridgeWith({ bootstrap })} />);

    expect(await screen.findByRole("heading", { name: "Workspace needs attention" })).toBeTruthy();
    expect(screen.getByRole("alert").textContent).not.toContain("/private");
    expect(document.querySelector(".statusbar")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("heading", { name: "Open a workspace" })).toBeTruthy();
    expect(bootstrap).toHaveBeenCalledTimes(2);
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
                  sessionRef: "invalid.jsonl",
                  sourceState: "invalid",
                  sourceBytes: 256,
                  sourceModifiedAtUnixMs: 1_784_419_100_000,
                  title: "Invalid session",
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
    expect(await screen.findByText("Invalid session")).toBeTruthy();
    expect(cursors).toEqual([undefined, "cursor-2"]);
    expect(screen.getAllByText("Invalid")).toHaveLength(1);

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

  it("refreshes the catalog before retrying a still-ready failed conversation", async () => {
    const user = userEvent.setup();
    const entry = {
      sessionRef: "retry.jsonl",
      sessionId: "durable-retry",
      sourceState: "ready" as const,
      sourceBytes: 512,
      sourceModifiedAtUnixMs: 1_784_419_200_000,
      title: "Retry this conversation",
      userMessageCount: 1,
      assistantMessageCount: 1,
      toolResultCount: 0,
      pinned: false,
    };
    const catalog = vi.fn(async () => ({ ...emptyCatalog, entries: [entry] }));
    const openSession = vi.fn()
      .mockRejectedValueOnce(new Error("temporary open failure"))
      .mockResolvedValueOnce({ id: "adapter-retry", label: entry.title, runCount: 1 });
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      catalog,
      openSession,
    })} />);

    await user.click(await screen.findByRole("button", { name: /^Retry this conversation/ }));
    expect(await screen.findByRole("button", { name: "Retry conversation" })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Retry conversation" }));
    expect(await screen.findByRole("heading", { name: "Retry this conversation" })).toBeTruthy();
    expect(openSession).toHaveBeenCalledTimes(2);
    expect(openSession).toHaveBeenNthCalledWith(1, workspace.id, {
      sessionRef: entry.sessionRef,
      sessionId: entry.sessionId,
      label: entry.title,
    });
    expect(openSession).toHaveBeenNthCalledWith(2, workspace.id, {
      sessionRef: entry.sessionRef,
      sessionId: entry.sessionId,
      label: entry.title,
    });
    expect(catalog).toHaveBeenCalledTimes(2);
  });

  it("stops retrying when a failed conversation refreshes as an invalid source", async () => {
    const user = userEvent.setup();
    const readyEntry = {
      sessionRef: "incompatible.jsonl",
      sessionId: "durable-incompatible",
      sourceState: "ready" as const,
      sourceBytes: 512,
      sourceModifiedAtUnixMs: 1_784_419_200_000,
      title: "Incompatible conversation",
      userMessageCount: 1,
      assistantMessageCount: 1,
      toolResultCount: 0,
      pinned: false,
    };
    const invalidEntry = {
      ...readyEntry,
      sessionId: undefined,
      sourceState: "invalid" as const,
      sourceDiagnostic: "invalid_event_stream" as const,
    };
    const catalog = vi.fn()
      .mockResolvedValueOnce({ ...emptyCatalog, entries: [readyEntry] })
      .mockResolvedValueOnce({ ...emptyCatalog, generation: 2, entries: [invalidEntry] });
    const openSession = vi.fn(async () => Promise.reject(new Error("not ready")));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      catalog,
      openSession,
    })} />);

    await user.click(await screen.findByRole("button", { name: /^Incompatible conversation/ }));

    expect(await screen.findByText("Conversation history refreshed because the list changed.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Retry conversation" })).toBeNull();
    expect(openSession).toHaveBeenCalledTimes(1);
    expect(catalog).toHaveBeenCalledTimes(2);
  });

  it("previews and executes selected conversation mutations through the batch contract", async () => {
    const user = userEvent.setup();
    let deleted = false;
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
    const executeSessionCatalogBatch = vi.fn(async () => {
      deleted = true;
      return {
        planId: "sha256:batch-ready",
        action: "delete_sessions" as const,
        total: 1,
        completed: 1,
        failed: 0,
        skipped: 0,
        items: [{ sessionRef: entry.sessionRef, outcome: "completed" as const }],
      };
    });
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async () => deleted
        ? { ...emptyCatalog, generation: 8, entries: [] }
        : { ...emptyCatalog, generation: 7, entries: [entry], nextCursor: "cursor-2" },
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
    expect(await screen.findByText("No conversations match these filters.")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Back to conversations" }));
    await waitFor(() => expect(screen.queryByText("Batch ready session")).toBeNull());
  });

  it("offers source cleanup for a selected invalid source", async () => {
    const user = userEvent.setup();
    const entry = {
      sessionRef: "invalid.jsonl",
      sourceState: "invalid" as const,
      sourceDiagnostic: "invalid_event_stream" as const,
      sourceBytes: 256,
      sourceModifiedAtUnixMs: 1_784_419_100_000,
      title: "Invalid session",
      userMessageCount: 0,
      assistantMessageCount: 0,
      toolResultCount: 0,
      pinned: false,
    };
    const planSessionCatalogBatch = vi.fn(async () => ({
      planId: "sha256:invalid-delete",
      action: "delete_invalid_sources" as const,
      generation: 8,
      total: 1,
      executable: 1,
      blocked: 0,
      items: [{ sessionRef: entry.sessionRef, status: "executable" as const }],
    }));
    const executeSessionCatalogBatch = vi.fn(async () => ({
      planId: "sha256:invalid-delete",
      action: "delete_invalid_sources" as const,
      total: 1,
      completed: 0,
      failed: 1,
      skipped: 0,
      items: [{
        sessionRef: entry.sessionRef,
        outcome: "failed" as const,
        reason: "writer_busy",
      }],
    }));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async () => ({ ...emptyCatalog, generation: 8, entries: [entry] }),
      planSessionCatalogBatch,
      executeSessionCatalogBatch,
    })} />);

    await screen.findByText("Invalid session");
    await user.click(screen.getByRole("button", { name: "Manage conversations" }));
    expect(await screen.findByText("Unreadable or identity-conflicting event stream")).toBeTruthy();
    await user.click(await screen.findByRole("checkbox", { name: "Select conversation: Invalid session" }));

    expect(screen.getByRole("button", { name: "Delete unavailable (1)" }).hasAttribute("disabled")).toBe(false);
    await user.click(screen.getByRole("button", { name: "Delete unavailable (1)" }));
    expect(planSessionCatalogBatch).toHaveBeenCalledWith(workspace.id, {
      action: "delete_invalid_sources",
      items: [{
        sessionRef: entry.sessionRef,
        sourceBytes: entry.sourceBytes,
        sourceModifiedAtUnixMs: entry.sourceModifiedAtUnixMs,
      }],
    });
    await user.click(await screen.findByRole("button", { name: "Apply action" }));
    expect(await screen.findByText("failed: In use by another Sigil instance")).toBeTruthy();
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
    let resolveDisplay: ((page: ConversationDisplayPage) => void) | undefined;
    let resolveRunContext: ((context: RunContext) => void) | undefined;
    let resolveContinuity: ((continuity: ConversationContinuity) => void) | undefined;
    const display = vi.fn(() => new Promise<ConversationDisplayPage>((resolve) => {
      resolveDisplay = resolve;
    }));
    const runContext = vi.fn(() => new Promise<RunContext>((resolve) => {
      resolveRunContext = resolve;
    }));
    const continuity = vi.fn(() => new Promise<ConversationContinuity>((resolve) => {
      resolveContinuity = resolve;
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
      display,
      runContext,
      continuity,
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
    await waitFor(() => expect(display).toHaveBeenCalledOnce());
    await waitFor(() => expect(runContext).toHaveBeenCalledOnce());
    expect(continuity).not.toHaveBeenCalled();
    expect(within(workspaceRegion).getByRole("status", { name: "Opening conversation…" })).toBe(loading);

    await act(async () => {
      resolveDisplay?.({
        schemaVersion: 1,
        requestScope: "loading-request-scope",
        throughSessionStreamSequence: "0",
        totalItems: "0",
        items: [],
        hasMore: false,
        gapFacts: [],
      });
    });
    await waitFor(() => expect(continuity).toHaveBeenCalledOnce());
    expect(within(workspaceRegion).getByRole("status", { name: "Opening conversation…" })).toBe(loading);
    expect(within(workspaceRegion).queryByRole("status", { name: "Checking conversation continuity" })).toBeNull();

    await act(async () => {
      resolveRunContext?.(defaultRunContext);
    });
    expect(within(workspaceRegion).getByRole("status", { name: "Opening conversation…" })).toBe(loading);
    expect(within(workspaceRegion).queryByRole("status", { name: "Checking conversation continuity" })).toBeNull();

    await act(async () => {
      resolveContinuity?.({
        durableFrontier: { throughStreamSequence: 0 },
        retainedTerminalRuns: [],
        recoveryActions: [],
      });
    });
    expect(await within(workspaceRegion).findByRole("heading", { name: "Loading state session" })).toBeTruthy();
    expect(within(workspaceRegion).queryByRole("status", { name: "Opening conversation…" })).toBeNull();
    expect((within(workspaceRegion).getByRole("combobox", { name: "Model" }) as HTMLSelectElement).value).toBe("deepseek-default/deepseek-v4-flash");
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

  it("opens canonical display items and automatically pages older messages at the top", async () => {
    const user = userEvent.setup();
    const displayQueries: Array<string | undefined> = [];
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
      display: async (_workspaceId, _sessionId, request) => {
        displayQueries.push(request.cursor);
        return request.cursor === undefined
          ? {
              schemaVersion: 1,
              requestScope: "history-request-scope",
              throughSessionStreamSequence: "4",
              totalItems: "4",
              items: [
                {
                  schemaVersion: 1,
                  displayId: "message-tool",
                  displayOrder: { sessionStreamSequence: "3", subindex: 0 },
                  sourceEventId: "event-message-tool",
                  kind: "tool",
                  source: "durable_run_event",
                  runId: "run-history",
                  runSequence: "3",
                  status: "succeeded",
                  content: {
                    type: "tool",
                    toolName: "shell",
                    output: "cargo test passed",
                    truncated: false,
                    originalContentBytes: 17,
                  },
                },
                {
                  schemaVersion: 1,
                  displayId: "message-final",
                  displayOrder: { sessionStreamSequence: "4", subindex: 0 },
                  sourceEventId: "event-message-final",
                  kind: "assistant_message",
                  source: "durable_transcript",
                  runId: "run-history",
                  status: "succeeded",
                  content: {
                    type: "message",
                    role: "assistant",
                    text: "The change is complete.",
                    assistantPhase: "final_answer",
                    imageAttachmentCount: 0,
                    truncated: false,
                    originalContentBytes: 23,
                  },
                },
              ],
              nextCursor: "cursor-before-3",
              hasMore: true,
              gapFacts: [],
            }
          : {
              schemaVersion: 1,
              requestScope: "history-request-scope",
              throughSessionStreamSequence: "4",
              totalItems: "4",
              items: [
                {
                  schemaVersion: 1,
                  displayId: "message-user",
                  displayOrder: { sessionStreamSequence: "1", subindex: 0 },
                  sourceEventId: "event-message-user",
                  kind: "user_message",
                  source: "durable_transcript",
                  runId: "run-history",
                  status: "recorded",
                  content: {
                    type: "message",
                    role: "user",
                    text: "Fix the parser",
                    imageAttachmentCount: 0,
                    truncated: false,
                    originalContentBytes: 14,
                  },
                },
                {
                  schemaVersion: 1,
                  displayId: "message-preamble",
                  displayOrder: { sessionStreamSequence: "2", subindex: 0 },
                  sourceEventId: "event-message-preamble",
                  kind: "assistant_message",
                  source: "durable_transcript",
                  runId: "run-history",
                  status: "recorded",
                  content: {
                    type: "message",
                    role: "assistant",
                    text: "I will inspect it.",
                    assistantPhase: "tool_preamble",
                    imageAttachmentCount: 0,
                    truncated: false,
                    originalContentBytes: 18,
                  },
                },
              ],
              hasMore: false,
              gapFacts: [],
            };
      },
    });
    render(<App bridge={bridge} />);

    expect(await screen.findByText("History with messages")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: /^History with messages/ }));
    expect(await screen.findByText("cargo test passed")).toBeTruthy();
    expect(screen.getByText("The change is complete.")).toBeTruthy();
    const timeline = screen.getByRole("log", { name: "Conversation timeline" });
    fireEvent.wheel(timeline);
    timeline.scrollTop = 0;
    fireEvent.scroll(timeline);
    const older = await screen.findByText("Fix the parser");
    const latest = screen.getByText("The change is complete.");
    expect(older.compareDocumentPosition(latest) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
    expect(displayQueries).toEqual([undefined, "cursor-before-3"]);
  });

  it("recovers an initial canonical display after a bounded transient retry", async () => {
    const user = userEvent.setup();
    const display = vi.fn()
      .mockRejectedValueOnce({
        code: "conversation_display_unavailable",
        message: "Canonical conversation history is temporarily unavailable.",
      })
      .mockResolvedValue({
        schemaVersion: 1,
        requestScope: "transient-display-scope",
        throughSessionStreamSequence: "2",
        totalItems: "1",
        items: [{
          schemaVersion: 1,
          displayId: "recovered-message",
          displayOrder: { sessionStreamSequence: "2", subindex: 0 },
          sourceEventId: "event-recovered-message",
          kind: "assistant_message",
          source: "durable_transcript",
          runId: "run-fast",
          status: "succeeded",
          content: {
            type: "message",
            role: "assistant",
            text: "Fast run recovered from the transient display window.",
            assistantPhase: "final_answer",
            imageAttachmentCount: 0,
            truncated: false,
            originalContentBytes: 53,
          },
        }],
        hasMore: false,
        gapFacts: [],
      });
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "transient-display.jsonl",
          sessionId: "durable-transient-display",
          sourceState: "ready",
          sourceBytes: 512,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Transient display session",
          userMessageCount: 1,
          assistantMessageCount: 1,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({
        id: "http-transient-display",
        label: "Transient display session",
        runCount: 1,
      }),
      display,
    });
    render(<App bridge={bridge} />);

    await user.click(await screen.findByRole("button", { name: /^Transient display session/ }));

    expect(await screen.findByText(
      "Fast run recovered from the transient display window.",
    )).toBeTruthy();
    expect(display).toHaveBeenCalledTimes(2);
    expect(screen.queryByText("Saved messages are unavailable")).toBeNull();
  });

  it("preserves a non-transient canonical display error without retrying it", async () => {
    const user = userEvent.setup();
    const display = vi.fn().mockRejectedValue({
      code: "conversation_display_cursor_invalid",
      message: "The canonical display cursor is invalid.",
    });
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "invalid-display.jsonl",
          sessionId: "durable-invalid-display",
          sourceState: "ready",
          sourceBytes: 512,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Invalid display session",
          userMessageCount: 1,
          assistantMessageCount: 0,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({
        id: "http-invalid-display",
        label: "Invalid display session",
        runCount: 1,
      }),
      display,
    });
    render(<App bridge={bridge} />);

    await user.click(await screen.findByRole("button", { name: /^Invalid display session/ }));

    expect(await screen.findByText("Saved messages are unavailable")).toBeTruthy();
    expect(screen.getByText("The canonical display cursor is invalid.")).toBeTruthy();
    expect(display).toHaveBeenCalledOnce();
  });

  it("reattaches an active run after listeners and restores bounded controls with honest gaps", async () => {
    const user = userEvent.setup();
    const order: string[] = [];
    const conversationQueue = vi.fn(async () => ({
      schemaVersion: 1,
      sessionId: "http-session-active",
      generation: "queue-v1:0:initial",
      paused: false,
      totalItems: 0,
      items: [],
      truncated: false,
    }));
    const commandConversationQueue = vi.fn(async (
      _workspaceId: string,
      input: ConversationQueueCommandInput,
    ) => ({
      commandId: "queue-command-active",
      clientId: "sigil-desktop",
      sessionId: input.sessionId,
      action: input.action.action,
      expectedGeneration: input.expectedGeneration,
      generation: "queue-v1:1:queued",
      queue: {
        schemaVersion: 1,
        sessionId: input.sessionId,
        generation: "queue-v1:1:queued",
        paused: false,
        totalItems: 1,
        items: [{
          entryId: "queue-entry-active",
          order: 0,
          kind: "chat" as const,
          status: "queued" as const,
          promptPreview: "Follow up after approval",
          promptPreviewTruncated: false,
          promptMaterial: "persisted_safe" as const,
          dispatchable: false,
          blockedReason: "foreground_run_active" as const,
        }],
        truncated: false,
      },
      replayed: false,
    }));
    let eventListener: ((event: TimelineEvent) => void) | undefined;
    let statusListener: ((status: RunStreamStatus) => void) | undefined;
    let cancelledRun = "";
    const activeEvent: TimelineEvent = {
      workspaceId: workspace.id,
      sessionId: "http-session-active",
      runId: "run-active",
      sequence: 1, runSequence: "1",
      provisionalId: "live-active-user",
      replayable: true,
      kind: "run_started",
      text: "Resume this work",
    };
    const approvalEvent: TimelineEvent = {
      ...activeEvent,
      sequence: 2, runSequence: "2",
      provisionalId: "live-active-approval",
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
      continuity: async () => ({
        durableFrontier: { throughStreamSequence: 2 },
        foregroundOwner: {
          runId: "run-active",
          ownerRevision: `sha256:${"a".repeat(64)}`,
        },
        retainedTerminalRuns: [],
        recoveryActions: ["retry_current", "continue_read_only"],
      }),
      conversationQueue,
      commandConversationQueue,
      subscribeRunEvents: async (listener) => {
        order.push("events");
        eventListener = listener;
        return () => undefined;
      },
      subscribeRunStreamStatus: async (listener) => {
        order.push("status");
        statusListener = listener;
        return () => undefined;
      },
      attachRun: async (_workspaceId, input) => {
        order.push("attach");
        expect(input).toEqual({
          sessionId: "http-session-active",
          runId: "run-active",
          ownerRevision: `sha256:${"a".repeat(64)}`,
        });
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

    await waitFor(() => expect(conversationQueue).toHaveBeenCalled());
    const composer = screen.getByRole("combobox", { name: "Message Sigil" });
    await user.type(composer, "Follow up after approval");
    fireEvent.keyDown(composer, { key: "Enter" });
    await waitFor(() => expect(commandConversationQueue).toHaveBeenCalledWith(
      workspace.id,
      {
        sessionId: "http-session-active",
        expectedGeneration: "queue-v1:0:initial",
        action: {
          action: "enqueue",
          prompt: "Follow up after approval",
          kind: "chat",
          reasoningEffort: undefined,
        },
      },
    ));
    expect(screen.getByText("Review the resumed edit")).toBeTruthy();

    act(() => eventListener?.(activeEvent));
    expect(screen.getAllByText("Resume this work")).toHaveLength(1);
    await user.click(screen.getByRole("button", { name: "Stop run" }));
    expect(cancelledRun).toBe("run-active");

    act(() => statusListener?.({
      workspaceId: workspace.id,
      sessionId: "http-session-active",
      runId: "run-active",
      state: "error",
      message: "Live progress is recovering.",
    }));
    expect(await screen.findByText("Live controls need attention")).toBeTruthy();
    await user.type(composer, "Queue while live controls recover");
    fireEvent.keyDown(composer, { key: "Enter" });
    await waitFor(() => expect(commandConversationQueue).toHaveBeenLastCalledWith(
      workspace.id,
      {
        sessionId: "http-session-active",
        expectedGeneration: "queue-v1:0:initial",
        action: {
          action: "enqueue",
          prompt: "Queue while live controls recover",
          kind: "chat",
          reasoningEffort: undefined,
        },
      },
    ));
  });

  it.each(["event", "status"] as const)(
    "blocks and re-probes when an unexpected nonterminal run %s arrives after an idle proof",
    async (signal) => {
      const user = userEvent.setup();
      let eventListener: ((event: TimelineEvent) => void) | undefined;
      let statusListener: ((status: RunStreamStatus) => void) | undefined;
      let resolveReprobe: ((continuity: ConversationContinuity) => void) | undefined;
      const owner: ConversationContinuity = {
        durableFrontier: { throughStreamSequence: 1 },
        foregroundOwner: {
          runId: "run-after-idle",
          ownerRevision: `sha256:${"f".repeat(64)}`,
        },
        retainedTerminalRuns: [],
        recoveryActions: ["retry_current", "continue_read_only"],
      };
      const continuity = vi.fn()
        .mockResolvedValueOnce({
          durableFrontier: { throughStreamSequence: 0 },
          retainedTerminalRuns: [],
          recoveryActions: [],
        })
        .mockImplementationOnce(() => new Promise<ConversationContinuity>((resolve) => {
          resolveReprobe = resolve;
        }))
        .mockResolvedValueOnce(owner);
      const attachRun = vi.fn(async () => ({
        run: {
          id: "run-after-idle",
          sessionId: "http-after-idle",
          status: "running" as const,
          permissionMode: "manual" as const,
          streamSequence: 1,
        },
        events: [],
        streamState: "live" as const,
        hasGap: false,
      }));
      render(<App bridge={bridgeWith({
        bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
        catalog: async () => ({
          ...emptyCatalog,
          entries: [{
            sessionRef: "after-idle.jsonl",
            sessionId: "durable-after-idle",
            sourceState: "ready",
            sourceBytes: 512,
            sourceModifiedAtUnixMs: 1_784_419_200_000,
            title: "After idle session",
            userMessageCount: 1,
            assistantMessageCount: 1,
            toolResultCount: 0,
            pinned: false,
          }],
        }),
        openSession: async () => ({ id: "http-after-idle", label: "After idle session", runCount: 1 }),
        continuity,
        attachRun,
        subscribeRunEvents: async (listener) => {
          eventListener = listener;
          return () => undefined;
        },
        subscribeRunStreamStatus: async (listener) => {
          statusListener = listener;
          return () => undefined;
        },
      })} />);

      await user.click(await screen.findByRole("button", { name: /^After idle session/ }));
      const send = await screen.findByRole("button", { name: "Send message" });
      await waitFor(() => expect(continuity).toHaveBeenCalledTimes(1));
      await user.type(screen.getByRole("combobox", { name: "Message Sigil" }), "Keep this draft pending");
      expect((send as HTMLButtonElement).disabled).toBe(false);
      await waitFor(() => {
        expect(eventListener).toBeDefined();
        expect(statusListener).toBeDefined();
      });

      act(() => {
        if (signal === "event") {
          eventListener?.({
            workspaceId: workspace.id,
            sessionId: "http-after-idle",
            runId: "run-after-idle",
            sequence: 1, runSequence: "1",
            provisionalId: "live-after-idle-user",
            replayable: false,
            kind: "run_started",
            text: "Work started elsewhere",
          });
        } else {
          statusListener?.({
            workspaceId: workspace.id,
            sessionId: "http-after-idle",
            runId: "run-after-idle",
            state: "live",
          });
        }
      });

      expect((send as HTMLButtonElement).disabled).toBe(true);
      await waitFor(() => expect(continuity).toHaveBeenCalledTimes(2));
      expect(attachRun).not.toHaveBeenCalled();

      await act(async () => {
        resolveReprobe?.(owner);
      });
      await waitFor(() => expect(attachRun).toHaveBeenCalledWith(workspace.id, {
        sessionId: "http-after-idle",
        runId: "run-after-idle",
        ownerRevision: owner.foregroundOwner?.ownerRevision,
      }));
      await waitFor(() => expect(continuity).toHaveBeenCalledTimes(3));
      expect(await screen.findByRole("button", { name: "Stop run" })).toBeTruthy();
      expect(screen.queryByRole("button", { name: "Send message" })).toBeNull();
    },
  );

  it("re-probes a changed foreground owner before enabling the composer", async () => {
    const user = userEvent.setup();
    const continuity = vi.fn()
      .mockResolvedValueOnce({
        durableFrontier: { throughStreamSequence: 4 },
        foregroundOwner: {
          runId: "run-stale",
          ownerRevision: `sha256:${"b".repeat(64)}`,
        },
        retainedTerminalRuns: [],
        recoveryActions: ["retry_current", "continue_read_only"],
      })
      .mockResolvedValueOnce({
        durableFrontier: { throughStreamSequence: 5 },
        retainedTerminalRuns: [],
        recoveryActions: [],
      });
    const attachRun = vi.fn(async () => Promise.reject({
      code: "run_owner_changed",
      message: "The foreground owner changed.",
    }));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "owner-race.jsonl",
          sessionId: "durable-owner-race",
          sourceState: "ready",
          sourceBytes: 512,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Owner race session",
          userMessageCount: 1,
          assistantMessageCount: 1,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({ id: "http-owner-race", label: "Owner race session", runCount: 1 }),
      continuity,
      attachRun,
    })} />);

    await user.click(await screen.findByRole("button", { name: /^Owner race session/ }));
    const input = await screen.findByRole("combobox", { name: "Message Sigil" });
    await waitFor(() => expect(continuity).toHaveBeenCalledTimes(2));
    expect(attachRun).toHaveBeenCalledTimes(1);
    await user.type(input, "Continue safely");
    expect((screen.getByRole("button", { name: "Send message" }) as HTMLButtonElement).disabled).toBe(false);
    expect(screen.queryByText("Live controls need attention")).toBeNull();
  });

  it("rejects an attachment whose owner changes after attach and confirms a fresh idle frontier", async () => {
    const user = userEvent.setup();
    const owner = (runId: string, revision: string): ConversationContinuity => ({
      durableFrontier: { throughStreamSequence: 9 },
      foregroundOwner: { runId, ownerRevision: `sha256:${revision.repeat(64)}` },
      retainedTerminalRuns: [],
      recoveryActions: ["retry_current", "continue_read_only"],
    });
    const continuity = vi.fn()
      .mockResolvedValueOnce(owner("run-before-race", "d"))
      .mockResolvedValueOnce(owner("run-after-race", "e"))
      .mockResolvedValueOnce({
        durableFrontier: { throughStreamSequence: 10 },
        retainedTerminalRuns: [],
        recoveryActions: [],
      });
    const attachRun = vi.fn(async (_workspaceId, input) => ({
      run: {
        id: input.runId,
        sessionId: input.sessionId,
        status: "running" as const,
        permissionMode: "manual" as const,
        streamSequence: 1,
      },
      events: [],
      streamState: "live" as const,
      hasGap: false,
    }));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "post-attach-race.jsonl",
          sessionId: "durable-post-attach-race",
          sourceState: "ready",
          sourceBytes: 512,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Post attach race",
          userMessageCount: 1,
          assistantMessageCount: 0,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({ id: "http-post-attach-race", label: "Post attach race", runCount: 1 }),
      continuity,
      attachRun,
    })} />);

    await user.click(await screen.findByRole("button", { name: /^Post attach race/ }));
    await waitFor(() => expect(continuity).toHaveBeenCalledTimes(3));
    expect(attachRun).toHaveBeenCalledTimes(1);
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    await user.type(input, "No stale owner window");
    expect((screen.getByRole("button", { name: "Send message" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("keeps a failed live attachment read-only until an explicit fresh retry succeeds", async () => {
    const user = userEvent.setup();
    const continuity = vi.fn()
      .mockResolvedValueOnce({
        durableFrontier: { throughStreamSequence: 7 },
        foregroundOwner: {
          runId: "run-unreachable",
          ownerRevision: `sha256:${"c".repeat(64)}`,
        },
        retainedTerminalRuns: [],
        recoveryActions: ["retry_current", "continue_read_only"],
      })
      .mockResolvedValueOnce({
        durableFrontier: { throughStreamSequence: 8 },
        retainedTerminalRuns: [],
        recoveryActions: [],
      });
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "read-only-recovery.jsonl",
          sessionId: "durable-read-only-recovery",
          sourceState: "ready",
          sourceBytes: 512,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Recovery session",
          userMessageCount: 1,
          assistantMessageCount: 1,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({ id: "http-read-only-recovery", label: "Recovery session", runCount: 1 }),
      display: async () => ({
        schemaVersion: 1,
        requestScope: "recovery-request-scope",
        throughSessionStreamSequence: "1",
        totalItems: "1",
        items: [{
          schemaVersion: 1,
          displayId: "saved-answer",
          displayOrder: { sessionStreamSequence: "1", subindex: 0 },
          sourceEventId: "event-saved-answer",
          kind: "assistant_message",
          source: "durable_transcript",
          runId: "run-unreachable",
          status: "succeeded",
          content: {
            type: "message",
            role: "assistant",
            text: "Saved transcript remains readable.",
            assistantPhase: "final_answer",
            imageAttachmentCount: 0,
            truncated: false,
            originalContentBytes: 34,
          },
        }],
        hasMore: false,
        gapFacts: [],
      }),
      continuity,
      attachRun: async () => Promise.reject({
        code: "run_stream_unavailable",
        message: "The live stream is unavailable.",
      }),
    })} />);

    await user.click(await screen.findByRole("button", { name: /^Recovery session/ }));
    expect(await screen.findByText("Saved transcript remains readable.")).toBeTruthy();
    expect(await screen.findByText("Live controls need attention")).toBeTruthy();
    const input = screen.getByRole("combobox", { name: "Message Sigil" });
    expect((input as HTMLTextAreaElement).disabled).toBe(false);
    await user.type(input, "Keep this draft while recovering");
    expect((input as HTMLTextAreaElement).value).toBe("Keep this draft while recovering");
    expect((screen.getByRole("button", { name: "Send message" }) as HTMLButtonElement).disabled).toBe(true);

    await user.click(screen.getByRole("button", { name: "Continue read-only" }));
    expect(screen.getByText("Conversation is read-only")).toBeTruthy();
    expect((input as HTMLTextAreaElement).disabled).toBe(false);
    expect((input as HTMLTextAreaElement).value).toBe("Keep this draft while recovering");

    await user.click(screen.getByRole("button", { name: "Retry connection" }));
    await waitFor(() => expect(continuity).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByText("Conversation is read-only")).toBeNull());
    await waitFor(() => expect((input as HTMLTextAreaElement).disabled).toBe(false));
    expect((input as HTMLTextAreaElement).value).toBe("Keep this draft while recovering");
    expect((screen.getByRole("button", { name: "Send message" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("renders only the recovery actions projected by a failed attachment", async () => {
    const user = userEvent.setup();
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "bounded-recovery.jsonl",
          sessionId: "durable-bounded-recovery",
          sourceState: "ready",
          sourceBytes: 512,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Bounded recovery session",
          userMessageCount: 1,
          assistantMessageCount: 1,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({ id: "http-bounded-recovery", label: "Bounded recovery session", runCount: 1 }),
      continuity: async () => ({
        durableFrontier: { throughStreamSequence: 3 },
        foregroundOwner: {
          runId: "run-bounded-recovery",
          ownerRevision: `sha256:${"a".repeat(64)}`,
        },
        retainedTerminalRuns: [],
        recoveryActions: ["retry_current", "continue_read_only"],
      }),
      attachRun: async () => Promise.reject({
        code: "run_stream_unavailable",
        message: "The live stream is unavailable.",
        recoveryActions: ["retry_current"],
      }),
    })} />);

    await user.click(await screen.findByRole("button", { name: /^Bounded recovery session/ }));
    expect(await screen.findByText("Live controls need attention")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Retry connection" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Continue read-only" })).toBeNull();
  });

  it("renders session attachment busy separately and retries without losing the draft", async () => {
    const user = userEvent.setup();
    const runContext = vi.fn()
      .mockResolvedValueOnce({
        ...defaultRunContext,
        routeRecovery: {
          code: "session_already_active" as const,
          allowedActions: ["retry_session_attach" as const, "start_new_session" as const],
          recoveryBinding: `sha256:${"c".repeat(64)}`,
          retryable: true,
        },
      })
      .mockResolvedValue(defaultRunContext);
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "busy-route.jsonl",
          sessionId: "durable-busy-route",
          sourceState: "ready",
          sourceBytes: 512,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Busy route session",
          userMessageCount: 1,
          assistantMessageCount: 0,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({ id: "http-busy-route", label: "Busy route session", runCount: 0 }),
      runContext,
    })} />);

    await user.click(await screen.findByRole("button", { name: /^Busy route session/ }));
    expect(await screen.findByText(/active in another Sigil window/)).toBeTruthy();
    const composer = screen.getByRole("combobox", { name: "Message Sigil" });
    await user.type(composer, "keep this exact draft");
    await user.click(screen.getByRole("button", { name: "Retry attachment" }));

    await waitFor(() => expect(runContext).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByText(/active in another Sigil window/)).toBeNull());
    expect((composer as HTMLTextAreaElement).value).toBe("keep this exact draft");
  });

  it("does not revive stale session recovery after the canonical run context is clean", async () => {
    const user = userEvent.setup();
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "stale-session-recovery.jsonl",
          sessionId: "durable-stale-session-recovery",
          sourceState: "ready",
          sourceBytes: 512,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Recovered route session",
          userMessageCount: 1,
          assistantMessageCount: 0,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({
        id: "http-stale-session-recovery",
        label: "Recovered route session",
        runCount: 0,
        routeRecovery: {
          code: "session_already_active" as const,
          allowedActions: ["retry_session_attach" as const],
          recoveryBinding: `sha256:${"d".repeat(64)}`,
          retryable: true,
        },
      }),
      runContext: async () => defaultRunContext,
    })} />);

    await user.click(await screen.findByRole("button", { name: /^Recovered route session/ }));
    await screen.findByRole("combobox", { name: "Message Sigil" });
    await waitFor(() => {
      expect(screen.queryByText(/active in another Sigil window/)).toBeNull();
      expect(screen.queryByRole("button", { name: "Retry attachment" })).toBeNull();
    });
  });

  it("clears provider recovery after an exact retry starts successfully", async () => {
    const user = userEvent.setup();
    const unavailableContext: RunContext = {
      ...defaultRunContext,
      routeRecovery: {
        code: "provider_unavailable",
        allowedActions: ["retry_provider"],
        recoveryBinding: "",
        retryable: true,
      },
    };
    const runContext = vi.fn()
      .mockResolvedValueOnce(unavailableContext)
      .mockResolvedValueOnce(unavailableContext)
      .mockResolvedValue(defaultRunContext);
    const startRun = vi.fn()
      .mockRejectedValueOnce(new Error("provider unavailable"))
      .mockResolvedValue({
        id: "run-provider-retry",
        sessionId: "http-provider-retry",
        status: "running" as const,
        permissionMode: "manual" as const,
        streamSequence: 0,
      });
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "provider-retry.jsonl",
          sessionId: "durable-provider-retry",
          sourceState: "ready",
          sourceBytes: 512,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Provider retry session",
          userMessageCount: 0,
          assistantMessageCount: 0,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({ id: "http-provider-retry", label: "Provider retry session", runCount: 0 }),
      runContext,
      startRun,
    })} />);

    await user.click(await screen.findByRole("button", { name: /^Provider retry session/ }));
    expect(await screen.findByText("Provider setup must be completed before this conversation can run.")).toBeTruthy();
    const composer = screen.getByRole("combobox", { name: "Message Sigil" });
    await user.type(composer, "retry this exact request");
    await user.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() => expect(startRun).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(runContext).toHaveBeenCalledTimes(2));
    await user.click(screen.getByRole("button", { name: "Retry provider" }));

    await waitFor(() => expect(startRun).toHaveBeenCalledTimes(2));
    await waitFor(() => {
      expect(screen.queryByText("Provider setup must be completed before this conversation can run.")).toBeNull();
      expect(screen.queryByRole("button", { name: "Retry provider" })).toBeNull();
    });
  });

  it("routes every projected recovery action without inventing retry or read-only controls", async () => {
    const user = userEvent.setup();
    const pickWorkspace = vi.fn(async () => ({ cancelled: true as const }));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      pickWorkspace,
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "action-recovery.jsonl",
          sessionId: "durable-action-recovery",
          sourceState: "ready",
          sourceBytes: 512,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Action recovery session",
          userMessageCount: 1,
          assistantMessageCount: 1,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({ id: "http-action-recovery", label: "Action recovery session", runCount: 1 }),
      continuity: async () => ({
        durableFrontier: { throughStreamSequence: 3 },
        foregroundOwner: {
          runId: "run-action-recovery",
          ownerRevision: `sha256:${"b".repeat(64)}`,
        },
        retainedTerminalRuns: [],
        recoveryActions: ["open_another_workspace", "open_diagnostics", "show_details"],
      }),
      attachRun: async () => Promise.reject({
        code: "run_stream_unavailable",
        message: "The live stream is unavailable.",
      }),
    })} />);

    await user.click(await screen.findByRole("button", { name: /^Action recovery session/ }));
    expect(await screen.findByText("Live controls need attention")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Retry connection" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Continue read-only" })).toBeNull();
    expect(screen.getByRole("button", { name: "Open another workspace" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Open support and diagnostics" })).toBeTruthy();
    await user.click(screen.getByText("Show details"));
    expect(screen.getByText("owner_probe_failed")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Open another workspace" }));
    expect(pickWorkspace).toHaveBeenCalledTimes(1);
    await user.click(screen.getByRole("button", { name: "Open support and diagnostics" }));
    expect(await screen.findByRole("heading", { name: "Support & diagnostics" })).toBeTruthy();
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
    expect(await screen.findByRole("heading", { name: "Keep this conversation" })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Filters" }));
    await user.type(screen.getByRole("textbox", { name: "Provider" }), "deepseek");
    expect(screen.getByText("1 active filter")).toBeTruthy();
    const clearActiveFilters = screen.getByRole("button", { name: "Clear" });
    expect(clearActiveFilters.querySelector("svg")).toBeTruthy();
    expect(screen.queryByText("Clear")).toBeNull();
    await user.click(clearActiveFilters);
    expect(screen.queryByText("1 active filter")).toBeNull();
    expect(screen.getByRole("heading", { name: "Keep this conversation" })).toBeTruthy();
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
    await user.dblClick(managedRow);
    expect(screen.getByRole("dialog", { name: "Rename conversation" })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Rename conversation" })).toBeNull());

    fireEvent.contextMenu(screen.getByRole("button", { name: /^Managed conversation/ }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    const name = screen.getByRole("textbox", { name: "Conversation name" });
    fireEvent.change(name, { target: { value: "Readable name" } });
    expect((name as HTMLInputElement).value).toBe("Readable name");
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

  it("keeps the active conversation heading synchronized with the durable catalog title", async () => {
    let catalogTitle = "Managed conversation";
    const renameSession = vi.fn(async (
      _workspaceId: string,
      input: { sessionRef: string; sessionId: string; displayName: string },
    ) => {
      catalogTitle = input.displayName;
      return {
        sessionRef: input.sessionRef,
        sessionId: input.sessionId,
        projectionGeneration: 2,
      };
    });
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
          title: catalogTitle,
          userMessageCount: 1,
          assistantMessageCount: 1,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({
        id: "http-session-open",
        label: "Stale server label",
        runCount: 2,
      }),
      renameSession,
    });
    const user = userEvent.setup();
    render(<App bridge={bridge} />);

    await user.click(await screen.findByRole("button", { name: /^Managed conversation/ }));
    expect(await screen.findByRole("heading", { name: "Managed conversation" })).toBeTruthy();

    fireEvent.contextMenu(screen.getByRole("button", { name: /^Managed conversation/ }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Conversation name" }), {
      target: { value: "Readable active title" },
    });
    await user.click(screen.getByRole("button", { name: "Rename" }));

    expect(await screen.findByRole("heading", { name: "Readable active title" })).toBeTruthy();
    expect(screen.getByRole("button", { name: /^Readable active title/ })).toBeTruthy();
  });

  it("quarantines an unavailable source from its context menu after confirmation", async () => {
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
    await user.click(screen.getByRole("menuitem", { name: "Move unavailable source to quarantine" }));
    expect(screen.getByRole("alertdialog", { name: "Quarantine unavailable conversation source?" })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Move to quarantine" }));
    await waitFor(() => expect(quarantineSession).toHaveBeenCalledWith(workspace.id, {
      sessionRef: "broken.jsonl",
      sourceBytes: 17,
      sourceModifiedAtUnixMs: 1_784_419_200_000,
    }));
    expect((await screen.findByText("Unavailable conversation source moved to quarantine.")).closest(".sg-toast-success")).toBeTruthy();
  });

  it("permanently deletes an unavailable source only after explicit confirmation", async () => {
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
    await user.click(screen.getByRole("menuitem", { name: "Delete unavailable source" }));
    expect(deleteInvalidSessionSource).not.toHaveBeenCalled();
    expect(screen.getByRole("alertdialog", { name: "Delete unavailable conversation source?" })).toBeTruthy();
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
    await waitFor(() => expect(document.activeElement).toBe(keepRunning));
    await user.tab({ shift: true });
    await waitFor(() => expect(document.activeElement).toBe(interruptRuns));
    await user.tab();
    await waitFor(() => expect(document.activeElement).toBe(keepRunning));
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
    Object.defineProperties(timeline, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
    });
    fireEvent.pointerEnter(timeline);
    timeline.scrollTop = 72;
    fireEvent.scroll(timeline);
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
    const composer = await readyComposer();
    expect(screen.queryByText("Available")).toBeNull();
    await user.type(composer, "Say hello");
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
      eventListener?.({ ...base, sequence: 1, runSequence: "1", provisionalId: "live-hello-user", kind: "run_started", text: "Say hello" });
      eventListener?.({ ...base, sequence: 2, runSequence: "2", kind: "assistant_delta", text: "Hel" });
      eventListener?.({ ...base, sequence: 3, runSequence: "3", provisionalId: "live-hello-final", kind: "assistant_message", text: "Hello" });
      eventListener?.({ ...base, sequence: 4, runSequence: "4", kind: "run_finished", text: "Hello", replayable: true });
      statusListener?.({ ...base, state: "terminal" });
    });

    expect(screen.getAllByText("Hello")).toHaveLength(1);
    expect(screen.queryByText("complete")).toBeNull();
    expect(screen.queryByText("Completed")).toBeNull();
    expect(screen.getByText("Run finished. Review the final response and verification status.")).toBeTruthy();
  });

  it("does not re-subscribe when a terminal event is replayed during the initial continuity probe", async () => {
    const user = userEvent.setup();
    const continuity = vi.fn(async (): Promise<ConversationContinuity> => ({
      durableFrontier: { throughStreamSequence: 2 },
      retainedTerminalRuns: [],
      recoveryActions: [],
    }));
    let eventSubscriptionCount = 0;
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "terminal-replay.jsonl",
          sessionId: "durable-terminal-replay",
          sourceState: "ready",
          sourceBytes: 512,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Terminal replay",
          userMessageCount: 1,
          assistantMessageCount: 1,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({ id: "http-terminal-replay", label: "Terminal replay", runCount: 1 }),
      display: async () => ({
        schemaVersion: 1,
        requestScope: "terminal-replay-scope",
        throughSessionStreamSequence: "2",
        totalItems: "2",
        items: [{
          schemaVersion: 1,
          displayId: "terminal-replay-answer",
          displayOrder: { sessionStreamSequence: "1", subindex: 0 },
          sourceEventId: "terminal-replay-answer-event",
          kind: "assistant_message",
          source: "durable_transcript",
          runId: "run-terminal-replay",
          status: "succeeded",
          content: {
            type: "message",
            role: "assistant",
            text: "This run already finished.",
            assistantPhase: "final_answer",
            imageAttachmentCount: 0,
            truncated: false,
            originalContentBytes: 26,
          },
        }, {
          schemaVersion: 1,
          displayId: "terminal-replay-finished",
          displayOrder: { sessionStreamSequence: "2", subindex: 0 },
          sourceEventId: "terminal-replay-finished-event",
          kind: "terminal",
          source: "durable_run_event",
          runId: "run-terminal-replay",
          status: "succeeded",
          content: {
            type: "terminal",
            finalMessageId: "terminal-replay-answer",
            summaryTruncated: false,
          },
        }],
        terminalFrontier: {
          runId: "run-terminal-replay",
          sessionStreamSequence: "2",
          status: "succeeded",
        },
        hasMore: false,
        gapFacts: [],
      }),
      continuity,
      subscribeRunEvents: async (listener) => {
        eventSubscriptionCount += 1;
        listener({
          workspaceId: workspace.id,
          sessionId: "http-terminal-replay",
          runId: "run-terminal-replay",
          sequence: 2,
          runSequence: "2",
          replayable: true,
          kind: "run_finished",
        });
        return () => undefined;
      },
    })} />);

    await user.click(await screen.findByRole("button", { name: /^Terminal replay/ }));
    expect(await screen.findByText("This run already finished.")).toBeTruthy();
    await waitFor(() => expect(continuity).toHaveBeenCalledTimes(1));
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 100));
    });

    expect(eventSubscriptionCount).toBe(1);
    expect(continuity).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("status", { name: "Checking conversation continuity" })).toBeNull();
    await user.type(screen.getByRole("combobox", { name: "Message Sigil" }), "Continue normally");
    expect((screen.getByRole("button", { name: "Send message" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("reloads the durable projection when a newly started run finishes before its owner is observed", async () => {
    const user = userEvent.setup();
    let completed = false;
    const display = vi.fn(async (): Promise<ConversationDisplayPage> => ({
      schemaVersion: 1,
      requestScope: "fast-run-scope",
      throughSessionStreamSequence: completed ? "1" : "0",
      totalItems: completed ? "1" : "0",
      items: completed
        ? [{
            schemaVersion: 1,
            displayId: "fast-run-final",
            displayOrder: { sessionStreamSequence: "1", subindex: 0 },
            sourceEventId: "event-fast-run-final",
            kind: "assistant_message",
            source: "durable_transcript",
            runId: "run-fast-complete",
            status: "succeeded",
            content: {
              type: "message",
              role: "assistant",
              text: "Fast durable completion",
              assistantPhase: "final_answer",
              imageAttachmentCount: 0,
              truncated: false,
              originalContentBytes: 23,
            },
          }]
        : [],
      hasMore: false,
      gapFacts: [],
    }));
    const continuity = vi.fn(async (): Promise<ConversationContinuity> => ({
      durableFrontier: { throughStreamSequence: completed ? 1 : 0 },
      retainedTerminalRuns: [],
      recoveryActions: [],
    }));
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      display,
      continuity,
      startRun: async (_workspaceId, sessionId) => {
        completed = true;
        return {
          id: "run-fast-complete",
          sessionId,
          status: "running",
          permissionMode: "manual",
          streamSequence: 0,
        };
      },
    })} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    const composer = await readyComposer();
    await user.type(composer, "Finish before attach");
    await user.click(screen.getByRole("button", { name: "Send message" }));

    expect(await screen.findByText("Fast durable completion", {}, { timeout: 4_000 })).toBeTruthy();
    expect(display).toHaveBeenCalledTimes(2);
    expect(continuity.mock.calls.length).toBeGreaterThanOrEqual(6);
    expect(screen.queryByText("Finish before attach")).toBeNull();
  });

  it("keeps a status-only terminal run finalizing until its interrupted durable frontier is loaded", async () => {
    const user = userEvent.setup();
    let statusListener: ((status: RunStreamStatus) => void) | undefined;
    let resolveRefresh: ((page: ConversationDisplayPage) => void) | undefined;
    const owner = {
      runId: "run-status-only",
      ownerRevision: `sha256:${"d".repeat(64)}`,
    };
    const continuity = vi.fn()
      .mockResolvedValueOnce({
        durableFrontier: { throughStreamSequence: 0 },
        foregroundOwner: owner,
        retainedTerminalRuns: [],
        recoveryActions: [],
      })
      .mockResolvedValueOnce({
        durableFrontier: { throughStreamSequence: 0 },
        foregroundOwner: owner,
        retainedTerminalRuns: [],
        recoveryActions: [],
      })
      .mockResolvedValue({
        durableFrontier: { throughStreamSequence: 2 },
        retainedTerminalRuns: [],
        recoveryActions: [],
      });
    let displayCall = 0;
    const display = vi.fn(() => {
      displayCall += 1;
      if (displayCall === 1) {
        return Promise.resolve({
          schemaVersion: 1 as const,
          requestScope: "status-only-scope",
          throughSessionStreamSequence: "0",
          totalItems: "0",
          items: [],
          hasMore: false,
          gapFacts: [],
        });
      }
      return new Promise<ConversationDisplayPage>((resolve) => {
        resolveRefresh = resolve;
      });
    });
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "status-only.jsonl",
          sessionId: "durable-status-only",
          sourceState: "ready",
          sourceBytes: 512,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Status-only session",
          userMessageCount: 1,
          assistantMessageCount: 0,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({ id: "http-status-only", label: "Status-only session", runCount: 1 }),
      display,
      continuity,
      attachRun: async () => ({
        run: {
          id: owner.runId,
          sessionId: "http-status-only",
          status: "running",
          permissionMode: "manual",
          streamSequence: 0,
        },
        events: [],
        streamState: "live",
        hasGap: false,
      }),
      subscribeRunStreamStatus: async (listener) => {
        statusListener = listener;
        return () => undefined;
      },
    })} />);

    await user.click(await screen.findByRole("button", { name: /^Status-only session/ }));
    expect(await screen.findByRole("button", { name: "Stop run" })).toBeTruthy();
    await user.type(screen.getByRole("combobox", { name: "Message Sigil" }), "Next durable turn");
    await waitFor(() => expect(statusListener).toBeDefined());
    act(() => statusListener?.({
      workspaceId: workspace.id,
      sessionId: "http-status-only",
      runId: owner.runId,
      state: "terminal",
    }));

    await waitFor(() => expect(display).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(continuity).toHaveBeenCalledTimes(3));
    const send = await screen.findByRole("button", { name: "Send message" });
    expect((send as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getAllByText("Run finished. Review the final response and verification status.")).toHaveLength(2);

    await act(async () => {
      resolveRefresh?.({
        schemaVersion: 1,
        requestScope: "status-only-scope",
        throughSessionStreamSequence: "2",
        totalItems: "2",
        items: [{
          schemaVersion: 1,
          displayId: "status-only-final",
          displayOrder: { sessionStreamSequence: "1", subindex: 0 },
          sourceEventId: "event-status-only-final",
          kind: "assistant_message",
          source: "durable_transcript",
          runId: owner.runId,
          status: "succeeded",
          content: {
            type: "message",
            role: "assistant",
            text: "The interrupted run was saved exactly once.",
            assistantPhase: "final_answer",
            imageAttachmentCount: 0,
            truncated: false,
            originalContentBytes: 41,
          },
        }, {
          schemaVersion: 1,
          displayId: "status-only-terminal",
          displayOrder: { sessionStreamSequence: "2", subindex: 0 },
          sourceEventId: "event-status-only-terminal",
          kind: "terminal",
          source: "durable_run_event",
          runId: owner.runId,
          status: "interrupted",
          content: { type: "terminal", finalMessageId: "status-only-final", summaryTruncated: false },
        }],
        terminalFrontier: {
          runId: owner.runId,
          sessionStreamSequence: "2",
          status: "interrupted",
        },
        hasMore: false,
        gapFacts: [],
      });
    });

    expect(await screen.findByText("The interrupted run was saved exactly once.")).toBeTruthy();
    await waitFor(() => expect((send as HTMLButtonElement).disabled).toBe(false));
  });

  it("keeps a rejected canonical refresh fail-closed without discarding its live final", async () => {
    const user = userEvent.setup();
    let eventListener: ((event: TimelineEvent) => void) | undefined;
    const owner = {
      runId: "run-invalid-refresh",
      ownerRevision: `sha256:${"e".repeat(64)}`,
    };
    const continuity = vi.fn()
      .mockResolvedValueOnce({
        durableFrontier: { throughStreamSequence: 1 },
        foregroundOwner: owner,
        retainedTerminalRuns: [],
        recoveryActions: ["retry_current"],
      })
      .mockResolvedValueOnce({
        durableFrontier: { throughStreamSequence: 1 },
        foregroundOwner: owner,
        retainedTerminalRuns: [],
        recoveryActions: ["retry_current"],
      })
      .mockResolvedValue({
        durableFrontier: { throughStreamSequence: 3 },
        retainedTerminalRuns: [],
        recoveryActions: ["retry_current"],
      });
    const display = vi.fn()
      .mockResolvedValueOnce({
        schemaVersion: 1,
        requestScope: "invalid-refresh-scope",
        throughSessionStreamSequence: "1",
        totalItems: "1",
        items: [{
          schemaVersion: 1,
          displayId: "stable-message",
          displayOrder: { sessionStreamSequence: "1", subindex: 0 },
          sourceEventId: "event-stable-message",
          kind: "user_message",
          source: "durable_transcript",
          status: "recorded",
          content: {
            type: "message",
            role: "user",
            text: "Original durable content",
            imageAttachmentCount: 0,
            truncated: false,
            originalContentBytes: 24,
          },
        }],
        hasMore: false,
        gapFacts: [],
      })
      .mockResolvedValue({
        schemaVersion: 1,
        requestScope: "invalid-refresh-scope",
        throughSessionStreamSequence: "3",
        totalItems: "2",
        items: [{
          schemaVersion: 1,
          displayId: "stable-message",
          displayOrder: { sessionStreamSequence: "1", subindex: 0 },
          sourceEventId: "event-stable-message",
          kind: "user_message",
          source: "durable_transcript",
          status: "recorded",
          content: {
            type: "message",
            role: "user",
            text: "Conflicting durable content",
            imageAttachmentCount: 0,
            truncated: false,
            originalContentBytes: 27,
          },
        }, {
          schemaVersion: 1,
          displayId: "invalid-refresh-terminal",
          displayOrder: { sessionStreamSequence: "3", subindex: 0 },
          sourceEventId: "event-invalid-refresh-terminal",
          kind: "terminal",
          source: "durable_run_event",
          runId: owner.runId,
          status: "succeeded",
          content: { type: "terminal", finalMessageId: "live-invalid-final", summaryTruncated: false },
        }],
        terminalFrontier: {
          runId: owner.runId,
          sessionStreamSequence: "3",
          status: "succeeded",
        },
        hasMore: false,
        gapFacts: [],
      });
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "invalid-refresh.jsonl",
          sessionId: "durable-invalid-refresh",
          sourceState: "ready",
          sourceBytes: 512,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Invalid refresh session",
          userMessageCount: 1,
          assistantMessageCount: 0,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({ id: "http-invalid-refresh", label: "Invalid refresh session", runCount: 1 }),
      display,
      continuity,
      attachRun: async () => ({
        run: {
          id: owner.runId,
          sessionId: "http-invalid-refresh",
          status: "running",
          permissionMode: "manual",
          streamSequence: 1,
        },
        events: [],
        streamState: "live",
        hasGap: false,
      }),
      subscribeRunEvents: async (listener) => {
        eventListener = listener;
        return () => undefined;
      },
    })} />);

    await user.click(await screen.findByRole("button", { name: /^Invalid refresh session/ }));
    expect(await screen.findByText("Original durable content")).toBeTruthy();
    await user.type(screen.getByRole("combobox", { name: "Message Sigil" }), "Do not enable yet");
    await waitFor(() => expect(eventListener).toBeDefined());
    act(() => {
      eventListener?.({
        workspaceId: workspace.id,
        sessionId: "http-invalid-refresh",
        runId: owner.runId,
        sequence: 2,
        runSequence: "2",
        provisionalId: "live-invalid-final",
        replayable: false,
        kind: "assistant_message",
        assistantKind: "final_answer",
        text: "Live final must remain visible",
      });
      eventListener?.({
        workspaceId: workspace.id,
        sessionId: "http-invalid-refresh",
        runId: owner.runId,
        sequence: 3,
        runSequence: "3",
        replayable: true,
        kind: "run_finished",
      });
    });

    expect(await screen.findByText("Live final must remain visible")).toBeTruthy();
    expect(await screen.findByText("Saved messages are unavailable")).toBeTruthy();
    expect(screen.getByText(/stable-message was reused with different content/)).toBeTruthy();
    await waitFor(() => expect(continuity).toHaveBeenCalledTimes(3));
    expect(screen.getByText("Live final must remain visible")).toBeTruthy();
    expect(screen.getByText("Original durable content")).toBeTruthy();
    expect(screen.queryByText("Conflicting durable content")).toBeNull();
    expect((screen.getByRole("button", { name: "Send message" }) as HTMLButtonElement).disabled).toBe(true);
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
    const composer = await readyComposer();
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
    expect((await screen.findByRole("combobox", { name: "Model" }) as HTMLSelectElement).value).toBe("deepseek-default/deepseek-v4-flash");
    expect(screen.getByRole("meter", { name: "Context usage 3%" })).toBeTruthy();
    const composer = await readyComposer();
    await user.selectOptions(screen.getByRole("combobox", { name: "Permission mode" }), "read-only");
    await user.selectOptions(screen.getByRole("combobox", { name: "Reasoning effort" }), "high");
    Object.defineProperty(composer, "scrollHeight", { configurable: true, value: 240 });
    await user.type(composer, "Inspect safely");
    expect(composer.style.height).toBe("176px");
    expect(composer.style.overflowY).toBe("auto");
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(selectedMode).toBe("read-only");
    expect(selectedEffort).toBe("high");
    expect(selectedEffortBinding).toBe("effort-binding-deepseek-v4-flash");
  });

  it("starts a cross-provider run in the current conversation", async () => {
    const user = userEvent.setup();
    const selectedModels: Array<RunContext["modelRef"] | undefined> = [];
    const runSelections: Array<{
      sessionId: string;
      modelRef?: RunContext["modelRef"];
      binding?: string;
      effort?: string;
      effortBinding?: string;
    }> = [];
    const gatewayOption: RunContext["modelOptions"][number] = {
      ...defaultRunContext.modelOptions[0],
      modelRef: { connectionId: "gateway-team", modelId: "deepseek-v4-flash" },
      displayName: "Gateway Flash",
      provenance: "remote",
      reasoningEffortBinding: "effort-binding-gateway-flash",
    };
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      providerConnections: async () => ({
        configMode: "v2",
        defaultModel: defaultRunContext.modelRef,
        connections: [
          {
            id: "deepseek-default",
            label: "DeepSeek",
            providerLabel: "DeepSeek",
            protocolLabel: "DeepSeek",
            endpointDisplay: "api.deepseek.com",
            credentialSource: "environment",
            readiness: "ready",
            defaultModel: defaultRunContext.modelRef,
          },
          {
            id: "gateway-team",
            label: "Team gateway",
            providerLabel: "OpenAI-compatible",
            protocolLabel: "Chat Completions",
            endpointDisplay: "gateway.example",
            credentialSource: "stored",
            readiness: "ready",
          },
        ],
        issues: [],
      }),
      runContext: async () => ({
        ...defaultRunContext,
        modelOptions: [defaultRunContext.modelOptions[0], gatewayOption],
      }),
      createSession: async () => {
        selectedModels.push(undefined);
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
        modelRef,
        modelSelectionBinding,
        reasoningEffort,
        reasoningEffortBinding,
      ) => {
        runSelections.push({
          sessionId,
          modelRef,
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
    const composer = await readyComposer();
    await user.selectOptions(
      await screen.findByRole("combobox", { name: "Model" }),
      "gateway-team/deepseek-v4-flash",
    );
    expect((screen.getByRole("combobox", { name: "Reasoning effort" }) as HTMLSelectElement).value).toBe("max");
    expect(screen.queryByRole("option", { name: "Effort unavailable" })).toBeNull();
    await user.type(composer, "Continue with pro");
    await user.click(screen.getByRole("button", { name: "Send message" }));

    expect(selectedModels).toEqual([undefined]);
    expect(runSelections).toEqual([{
      sessionId: "http-session-1",
      modelRef: {
        connectionId: "gateway-team",
        modelId: "deepseek-v4-flash",
      },
      binding: "model-binding-deepseek-v4-flash",
      effort: "max",
      effortBinding: "effort-binding-gateway-flash",
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
    const composer = await readyComposer();
    await user.type(composer, "Run this after Shift+Enter");
    fireEvent.keyDown(composer, { key: "Enter", shiftKey: true });
    expect(prompts).toEqual([]);
    fireEvent.keyDown(composer, { key: "Enter" });
    await waitFor(() => expect(prompts).toEqual(["Run this after Shift+Enter"]));
    await waitFor(() => expect(composer.disabled).toBe(false));
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
      eventListener?.({ ...base, sequence: 1, runSequence: "1", provisionalId: "live-scroll-user", kind: "run_started", text: "First" });
    });
    expect(timeline.scrollTop).toBe(600);

    timeline.scrollTop = 0;
    act(() => {
      eventListener?.({ ...base, sequence: 2, runSequence: "2", kind: "assistant_delta", text: "Growing" });
    });
    expect(timeline.scrollTop).toBe(600);

    fireEvent.wheel(timeline);
    timeline.scrollTop = 100;
    fireEvent.scroll(timeline);
    act(() => {
      eventListener?.({ ...base, sequence: 3, runSequence: "3", provisionalId: "live-scroll-answer", kind: "assistant_message", text: "Second" });
    });
    expect(timeline.scrollTop).toBe(100);
  });

  it("preserves the long-response viewport when focus moves outside the timeline", async () => {
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
      scrollHeight: { configurable: true, value: 900 },
    });
    const base = {
      workspaceId: workspace.id,
      sessionId: "http-session-new",
      runId: "run-long-final",
      replayable: false,
    };
    act(() => {
      eventListener?.({
        ...base,
        sequence: 1,
        runSequence: "1",
        provisionalId: "live-long-final",
        kind: "assistant_message",
        assistantKind: "final_answer",
        text: "A long final response",
      });
    });
    expect(await screen.findByText("A long final response")).toBeTruthy();

    fireEvent.pointerDown(timeline);
    timeline.scrollTop = 800;
    fireEvent.scroll(timeline);

    const composer = screen.getByRole("combobox", { name: "Message Sigil" });
    fireEvent.pointerDown(composer);
    timeline.scrollTop = 240;
    fireEvent.scroll(timeline);
    fireEvent.focus(composer);
    await waitFor(() => expect(timeline.scrollTop).toBe(800));

    fireEvent.wheel(timeline);
    timeline.scrollTop = 360;
    fireEvent.scroll(timeline);
    fireEvent.pointerDown(composer);
    timeline.scrollTop = 180;
    fireEvent.scroll(timeline);
    fireEvent.focus(composer);
    await waitFor(() => expect(timeline.scrollTop).toBe(360));
  });

  it("preserves the visible reader offset when canonical refresh reconciles a live item", async () => {
    const user = userEvent.setup();
    let eventListener: ((event: TimelineEvent) => void) | undefined;
    const owner = {
      runId: "run-scroll-anchor",
      ownerRevision: `sha256:${"f".repeat(64)}`,
    };
    const continuity = vi.fn()
      .mockResolvedValueOnce({
        durableFrontier: { throughStreamSequence: 1 },
        foregroundOwner: owner,
        retainedTerminalRuns: [],
        recoveryActions: [],
      })
      .mockResolvedValueOnce({
        durableFrontier: { throughStreamSequence: 1 },
        foregroundOwner: owner,
        retainedTerminalRuns: [],
        recoveryActions: [],
      })
      .mockResolvedValue({
        durableFrontier: { throughStreamSequence: 3 },
        retainedTerminalRuns: [],
        recoveryActions: [],
      });
    let displayCall = 0;
    const display = vi.fn(async (): Promise<ConversationDisplayPage> => {
      displayCall += 1;
      const earlier = {
        schemaVersion: 1 as const,
        displayId: "anchor-earlier",
        displayOrder: { sessionStreamSequence: "1", subindex: 0 },
        sourceEventId: "event-anchor-earlier",
        kind: "user_message" as const,
        source: "durable_transcript" as const,
        status: "recorded" as const,
        content: {
          type: "message" as const,
          role: "user" as const,
          text: "Earlier saved prompt",
          imageAttachmentCount: 0,
          truncated: false,
          originalContentBytes: 20,
        },
      };
      if (displayCall === 1) {
        return {
          schemaVersion: 1,
          requestScope: "scroll-anchor-scope",
          throughSessionStreamSequence: "1",
          totalItems: "1",
          items: [earlier],
          hasMore: false,
          gapFacts: [],
        };
      }
      return {
        schemaVersion: 1,
        requestScope: "scroll-anchor-scope",
        throughSessionStreamSequence: "3",
        totalItems: "3",
        items: [earlier, {
          schemaVersion: 1,
          displayId: "durable-scroll-anchor",
          displayOrder: { sessionStreamSequence: "2", subindex: 0 },
          sourceEventId: "event-durable-scroll-anchor",
          kind: "assistant_message",
          source: "durable_transcript",
          runId: owner.runId,
          status: "succeeded",
          reconciles: ["live-scroll-anchor"],
          content: {
            type: "message",
            role: "assistant",
            text: "Anchored answer",
            assistantPhase: "final_answer",
            imageAttachmentCount: 0,
            truncated: false,
            originalContentBytes: 15,
          },
        }, {
          schemaVersion: 1,
          displayId: "durable-scroll-terminal",
          displayOrder: { sessionStreamSequence: "3", subindex: 0 },
          sourceEventId: "event-durable-scroll-terminal",
          kind: "terminal",
          source: "durable_run_event",
          runId: owner.runId,
          status: "succeeded",
          content: {
            type: "terminal",
            finalMessageId: "durable-scroll-anchor",
            summaryTruncated: false,
          },
        }],
        terminalFrontier: {
          runId: owner.runId,
          sessionStreamSequence: "3",
          status: "succeeded",
        },
        liveProvisionalAnchor: {
          durableFrontier: "3",
          runId: owner.runId,
          runSequence: "3",
        },
        hasMore: false,
        gapFacts: [],
      };
    });
    render(<App bridge={bridgeWith({
      bootstrap: async () => ({ protocolVersion: 2, workspaces: [workspace], recentWorkspaces: [] }),
      catalog: async () => ({
        ...emptyCatalog,
        entries: [{
          sessionRef: "scroll-anchor.jsonl",
          sessionId: "durable-scroll-anchor-session",
          sourceState: "ready",
          sourceBytes: 512,
          sourceModifiedAtUnixMs: 1_784_419_200_000,
          title: "Scroll anchor session",
          userMessageCount: 1,
          assistantMessageCount: 0,
          toolResultCount: 0,
          pinned: false,
        }],
      }),
      openSession: async () => ({ id: "http-scroll-anchor", label: "Scroll anchor session", runCount: 1 }),
      display,
      continuity,
      attachRun: async () => ({
        run: {
          id: owner.runId,
          sessionId: "http-scroll-anchor",
          status: "running",
          permissionMode: "manual",
          streamSequence: 1,
        },
        events: [],
        streamState: "live",
        hasGap: false,
      }),
      subscribeRunEvents: async (listener) => {
        eventListener = listener;
        return () => undefined;
      },
    })} />);

    await user.click(await screen.findByRole("button", { name: /^Scroll anchor session/ }));
    expect(await screen.findByText("Earlier saved prompt")).toBeTruthy();
    await waitFor(() => expect(eventListener).toBeDefined());
    act(() => {
      eventListener?.({
        workspaceId: workspace.id,
        sessionId: "http-scroll-anchor",
        runId: owner.runId,
        sequence: 2,
        runSequence: "2",
        provisionalId: "live-scroll-anchor",
        replayable: false,
        kind: "assistant_message",
        assistantKind: "final_answer",
        text: "Anchored answer",
      });
    });
    expect(await screen.findByText("Anchored answer")).toBeTruthy();

    const timeline = screen.getByRole("log", { name: "Conversation timeline" });
    Object.defineProperties(timeline, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
    });
    fireEvent.wheel(timeline);
    timeline.scrollTop = 300;
    fireEvent.scroll(timeline);
    const rect = (top: number, bottom: number): DOMRect => ({
      x: 0,
      y: top,
      top,
      bottom,
      left: 0,
      right: 500,
      width: 500,
      height: bottom - top,
      toJSON: () => ({}),
    });
    const geometry = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function getBounds(this: HTMLElement) {
      if (this === timeline) return rect(100, 500);
      switch (this.dataset.displayId) {
        case "anchor-earlier": return rect(40, 80);
        case "live-scroll-anchor": return rect(140, 180);
        case "durable-scroll-anchor": return rect(190, 230);
        default: return rect(0, 0);
      }
    });

    try {
      act(() => {
        eventListener?.({
          workspaceId: workspace.id,
          sessionId: "http-scroll-anchor",
          runId: owner.runId,
          sequence: 3,
          runSequence: "3",
          replayable: true,
          kind: "run_finished",
        });
      });

      await waitFor(() => expect(display).toHaveBeenCalledTimes(2));
      await waitFor(() => expect(document.querySelector('[data-display-id="durable-scroll-anchor"]')).not.toBeNull());
      expect(document.querySelector('[data-display-id="live-scroll-anchor"]')).toBeNull();
      expect(timeline.scrollTop).toBe(350);
    } finally {
      geometry.mockRestore();
    }
  });

  it("submits an approve_family decision with the derived pattern", async () => {
    const user = userEvent.setup();
    let eventListener: ((event: TimelineEvent) => void) | undefined;
    let approvedCall = "";
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
      resolveApproval: async (_workspaceId, sessionId, runId, approval, decision) => {
        approvedCall = `${runId}:${approval.approvalRequestId}:${decision}`;
        return {
          commandId: "approval-command-family",
          clientId: "desktop-test",
          sessionId,
          runId,
          callId: approval.callId,
          approvalRequestId: approval.approvalRequestId,
          expectedStreamSequence: 3,
          decision: "approved",
          routeState: "decision_accepted",
          registryRevision: 4,
          replayed: false,
        };
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    await user.type(await readyComposer(), "Run tests");
    await user.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() => expect(eventListener).toBeDefined());
    await readyComposer();
    act(() => {
      eventListener?.({
        workspaceId: workspace.id,
        sessionId: "http-session-new",
        runId: "run-1",
        sequence: 3, runSequence: "3",
        provisionalId: "live-approval-call-1",
        replayable: true,
        kind: "approval_requested",
        itemId: "call-1",
        toolName: "bash",
        approval: {
          callId: "call-1",
          toolName: "bash",
          approvalRequestId: "approval-1",
          toolCallHash: "hash-1",
          policyVersion: "policy-1",
          expiresAtMs: 4_102_444_800_000,
          sessionGrantAvailable: true,
          commandFamilyAllowPattern: "cargo test*",
          analysisStatus: "complete",
          containment: ["filesystem=workspace_read", "network=deny"],
          safeSummaryTitle: "Run cargo test",
          safeSummaryDetail: "Workspace validation",
          operation: "execute_workspace_check_command",
          risk: "medium",
          snapshotRequired: false,
        },
      });
    });

    await screen.findByText("Run cargo test");
    await user.click(screen.getByRole("button", { name: "Allow family" }));
    expect(approvedCall).toBe("run-1:approval-1:approve_family");
    expect(await screen.findByText("Approval accepted")).toBeTruthy();
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
      resolveApproval: async (_workspaceId, sessionId, runId, approval, decision) => {
        approvedCall = `${runId}:${approval.approvalRequestId}:${decision}`;
        return {
          commandId: "approval-command-1",
          clientId: "desktop-test",
          sessionId,
          runId,
          callId: approval.callId,
          approvalRequestId: approval.approvalRequestId,
          expectedStreamSequence: 3,
          decision: decision === "deny"
            ? "denied"
            : decision === "approve_session" ? "approved_for_session" : "approved",
          routeState: "decision_accepted",
          registryRevision: 4,
          replayed: false,
        };
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
    await user.type(await readyComposer(), "Edit a file");
    await user.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() => expect(eventListener).toBeDefined());
    await readyComposer();
    await user.click(await screen.findByRole("button", { name: "Open verification: passed" }));
    expect(screen.getByRole("dialog", { name: "Verification" })).toBeTruthy();
    act(() => {
      eventListener?.({
        workspaceId: workspace.id,
        sessionId: "http-session-new",
        runId: "run-1",
        sequence: 3, runSequence: "3",
        provisionalId: "live-approval-call-1",
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
          effects: ["file_write", "execute_workspace_code"],
          subjects: ["workspace:README.md"],
          analysisStatus: "complete",
          containment: ["filesystem=workspace_write", "network=deny"],
          decisionReasons: ["workspace code execution requires an explicit decision"],
          safeSummaryTitle: "Edit one file",
          safeSummaryDetail: "Writes README.md inside the workspace",
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
    expect(within(approvalDock).getByText("file_write, execute_workspace_code")).toBeTruthy();
    expect(within(approvalDock).getByText("workspace:README.md")).toBeTruthy();
    expect(within(approvalDock).getByText("filesystem=workspace_write · network=deny")).toBeTruthy();
    await user.click(within(approvalDock).getByText("Why this decision is required"));
    expect(within(approvalDock).getByText("workspace code execution requires an explicit decision")).toBeTruthy();
    expect(screen.queryByRole("dialog", { name: "Verification" })).toBeNull();
    expect(screen.getByRole("button", { name: "Open verification: passed" }).hasAttribute("disabled")).toBe(true);
    expect(document.activeElement).toBe(approvalDock);
    fireEvent.keyDown(approvalDock, { key: "Escape" });
    expect(document.activeElement).toBe(screen.getByLabelText("Message Sigil"));
    await user.click(screen.getByRole("button", { name: "Allow for session" }));
    expect(approvedCall).toBe("run-1:approval-1:approve_session");
    expect(await screen.findByText("Approval accepted")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Allow for session" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Approve once" })).toBeNull();
    expect(document.activeElement).toBe(screen.getByLabelText("Message Sigil"));
    act(() => {
      eventListener?.({
        workspaceId: workspace.id,
        sessionId: "http-session-new",
        runId: "run-1",
        sequence: 4, runSequence: "4",
        provisionalId: "live-approval-call-1",
        replayable: true,
        kind: "approval_resolved",
        itemId: "call-1",
        approvalRequestId: "approval-1",
        status: "approved",
      });
    });
    expect(screen.queryByText("Edit one file")).toBeNull();
    expect(document.activeElement).toBe(screen.getByLabelText("Message Sigil"));
    act(() => {
      eventListener?.({
        workspaceId: workspace.id,
        sessionId: "http-session-new",
        runId: "run-1",
        sequence: 5, runSequence: "5",
        replayable: true,
        kind: "approval_requested",
        itemId: "call-2",
        toolName: "bash",
        approval: {
          callId: "call-2",
          toolName: "bash",
          approvalRequestId: "approval-2",
          toolCallHash: "hash-2",
          policyVersion: "policy-1",
          expiresAtMs: 4_102_444_800_000,
          sessionGrantAvailable: false,
          sessionGrantUnavailableReason: { code: "risk_not_grantable" },
          safeSummaryTitle: "Run unknown command",
          safeSummaryDetail: "Runs one high-risk command",
          risk: "high",
          snapshotRequired: false,
        },
      });
    });
    const unavailableApproval = (await screen.findByText("Run unknown command")).closest("section") as HTMLElement;
    expect(within(unavailableApproval).getByText("Session approval is unavailable at this request's risk level.")).toBeTruthy();
    expect(within(unavailableApproval).queryByRole("button", { name: "Allow for session" })).toBeNull();
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
          requestId: `verification-rerun-${"a".repeat(64)}`,
          taskId: "task_1",
          planVersion: 1,
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

  it("projects typed Task progress and continues the exact Task with optional guidance", async () => {
    const user = userEvent.setup();
    let eventListener: ((event: TimelineEvent) => void) | undefined;
    const continueTask = vi.fn(async (
      _workspaceId: string,
      sessionId: string,
      _taskId: string,
      permissionMode: PermissionMode,
    ) => ({
      id: "run-task-continue",
      sessionId,
      status: "running" as const,
      permissionMode,
      streamSequence: 0,
    }));
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
      continueTask,
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    await waitFor(() => expect(eventListener).toBeDefined());
    const base = {
      workspaceId: workspace.id,
      sessionId: "http-session-new",
      runId: "run-task-1",
      replayable: true,
    };
    act(() => {
      eventListener?.({
        ...base,
        sequence: 1,
        runSequence: "1",
        kind: "task_run_started",
        status: "running",
        task: { taskId: "task-1", objective: "Implement durable Task controls" },
      });
      eventListener?.({
        ...base,
        sequence: 2,
        runSequence: "2",
        kind: "task_plan_updated",
        status: "approved",
        task: {
          taskId: "task-1",
          planVersion: 2,
          steps: [{
            stepId: "step-1",
            title: "Inspect the bridge",
            role: "explorer",
            dependsOn: [],
            mode: "read_only",
            isolation: "shared",
          }],
        },
      });
      eventListener?.({
        ...base,
        sequence: 3,
        runSequence: "3",
        kind: "task_step_changed",
        status: "completed",
        task: { taskId: "task-1", planVersion: 2, stepId: "step-1" },
      });
      eventListener?.({
        ...base,
        sequence: 4,
        runSequence: "4",
        kind: "task_run_finished",
        status: "interrupted",
        task: { taskId: "task-1" },
      });
    });

    expect(await screen.findByText("Implement durable Task controls")).toBeTruthy();
    expect(screen.getByText("Inspect the bridge")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Add guidance" }));
    await user.type(
      screen.getByRole("textbox", { name: "Guidance for this Task" }),
      "Keep the public API compatible.",
    );
    await user.click(screen.getByRole("button", { name: "Continue with guidance" }));
    await waitFor(() => expect(continueTask).toHaveBeenCalledWith(
      workspace.id,
      "http-session-new",
      "task-1",
      "manual",
      "Keep the public API compatible.",
    ));
  });

  it("restores paused Task controls from the canonical durable display without live events", async () => {
    const user = userEvent.setup();
    const continueTask = vi.fn(async (
      _workspaceId: string,
      sessionId: string,
      _taskId: string,
      permissionMode: PermissionMode,
    ) => ({
      id: "run-task-restarted",
      sessionId,
      status: "running" as const,
      permissionMode,
      streamSequence: 0,
    }));
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      display: async () => ({
        schemaVersion: 1,
        requestScope: "http-session-new",
        throughSessionStreamSequence: "17",
        totalItems: "0",
        items: [],
        hasMore: false,
        gapFacts: [],
        taskControl: {
          schemaVersion: 1,
          taskId: "task-restart-control",
          phase: "execution",
          status: "paused",
          planVersion: 3,
          planStatus: "accepted",
          steps: [{
            stepId: "resume-step",
            title: "Resume after application restart",
            role: "executor",
            dependsOn: [],
            mode: "write",
            isolation: "worktree",
            status: "interrupted",
          }],
          stepsTruncated: false,
          activeChildren: 0,
          completedChildren: 1,
          failedChildren: 0,
          lanes: [],
          lanesTruncated: false,
          canContinue: true,
        },
      }),
      continueTask,
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));

    expect(await screen.findByText("task-restart-control")).toBeTruthy();
    expect(screen.getByText("Resume after application restart")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Continue Task" }));
    await waitFor(() => expect(continueTask).toHaveBeenCalledWith(
      workspace.id,
      "http-session-new",
      "task-restart-control",
      "manual",
      undefined,
    ));
  });

  it("pauses the exact active Task plan without using ordinary run cancellation", async () => {
    const user = userEvent.setup();
    let eventListener: ((event: TimelineEvent) => void) | undefined;
    const pauseTask = vi.fn(async (
      _workspaceId: string,
      sessionId: string,
      runId: string,
    ) => ({
      id: runId,
      sessionId,
      status: "paused" as const,
      permissionMode: "manual" as const,
      streamSequence: 4,
    }));
    const cancelRun = vi.fn();
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
      pauseTask,
      cancelRun,
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    await user.type(await readyComposer(), "Implement the active Task safely");
    await user.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() => expect(eventListener).toBeDefined());
    act(() => {
      eventListener?.({
        workspaceId: workspace.id,
        sessionId: "http-session-new",
        runId: "run-1",
        sequence: 1,
        runSequence: "1",
        replayable: true,
        kind: "task_run_started",
        status: "running",
        task: { taskId: "task-pause-1", objective: "Pause without losing the plan" },
      });
      eventListener?.({
        workspaceId: workspace.id,
        sessionId: "http-session-new",
        runId: "run-1",
        sequence: 2,
        runSequence: "2",
        replayable: true,
        kind: "task_plan_updated",
        status: "approved",
        task: {
          taskId: "task-pause-1",
          planVersion: 7,
          steps: [],
        },
      });
      eventListener?.({
        workspaceId: workspace.id,
        sessionId: "http-session-new",
        runId: "run-1",
        sequence: 3,
        runSequence: "3",
        replayable: true,
        kind: "approval_requested",
        itemId: "call-task-pause",
        toolName: "write_file",
        approval: {
          callId: "call-task-pause",
          toolName: "write_file",
          approvalRequestId: "approval-task-pause",
          toolCallHash: "hash-task-pause",
          policyVersion: "policy-task-pause",
          expiresAtMs: 4_102_444_800_000,
          snapshotRequired: true,
        },
      });
    });

    const pause = await screen.findByRole("button", { name: "Pause Task" });
    expect(pause.hasAttribute("disabled")).toBe(false);
    await user.click(pause);
    await waitFor(() => expect(pauseTask).toHaveBeenCalledWith(
      workspace.id,
      "http-session-new",
      "run-1",
      { taskId: "task-pause-1", planVersion: 7 },
    ));
    expect(cancelRun).not.toHaveBeenCalled();
  });

  it("renders the durable plan review card and submits the hash-bound Run decision", async () => {
    const user = userEvent.setup();
    const planHash = `sha256:${"d".repeat(64)}`;
    let displayCalls = 0;
    const planDecision = vi.fn(async (
      _workspaceId: string,
      sessionId: string,
      planId: string,
      expectedPlanHash: string,
      action: string,
    ) => ({
      commandId: "plan-decision-command-1",
      clientId: "desktop-test",
      sessionId,
      planId,
      planHash: expectedPlanHash,
      action: action as "run",
      replayed: false,
    }));
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      display: async () => {
        displayCalls += 1;
        return {
          schemaVersion: 1,
          requestScope: "http-session-new",
          throughSessionStreamSequence: "9",
          totalItems: "0",
          items: [],
          hasMore: false,
          gapFacts: [],
          planReview: displayCalls === 1 ? {
            planId: "plan-review-1",
            planHash,
            status: "draft_ready" as const,
            summary: "Refactor the workspace snapshot binding",
            summaryTruncated: false,
            stepCount: 3,
            targetPathCount: 2,
            suggestedCheckCount: 1,
            risk: "touches the recovery path",
            allowedActions: ["run", "save", "revise", "reject"] as const,
            source: "automatic_conversation_route" as const,
            stale: false,
          } : undefined,
        };
      },
      planDecision,
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));

    expect(await screen.findByText("Refactor the workspace snapshot binding")).toBeTruthy();
    expect(screen.getByText("plan-review-1")).toBeTruthy();
    expect(screen.getByText(/sha256:ddddd/)).toBeTruthy();
    expect(screen.getByText("Automatic plan review")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Review complete plan" }));
    expect(await screen.findByTestId("plan-workbench")).toBeTruthy();
    const runButton = screen.getByRole("button", { name: "Run plan" });
    expect(runButton.hasAttribute("disabled")).toBe(false);
    await user.click(runButton);

    await waitFor(() => expect(planDecision).toHaveBeenCalledWith(
      workspace.id,
      "http-session-new",
      "plan-review-1",
      planHash,
      "run",
    ));
    await waitFor(() => expect(screen.queryByText("Refactor the workspace snapshot binding")).toBeNull());
  });

  it("disables run and save but keeps revise and reject for a stale draft", async () => {
    const user = userEvent.setup();
    const planDecision = vi.fn(async () => ({
      commandId: "plan-decision-stale",
      clientId: "desktop-test",
      sessionId: "http-session-new",
      planId: "plan-review-stale",
      planHash: `sha256:${"e".repeat(64)}`,
      action: "revise" as const,
      revisionRunId: "plan-review-revise-run-1",
      replayed: false,
    }));
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      display: async () => ({
        schemaVersion: 1,
        requestScope: "http-session-new",
        throughSessionStreamSequence: "9",
        totalItems: "0",
        items: [],
        hasMore: false,
        gapFacts: [],
        planReview: {
          planId: "plan-review-stale",
          planHash: `sha256:${"e".repeat(64)}`,
          status: "draft_ready" as const,
          summary: "Stale plan draft",
          summaryTruncated: false,
          stepCount: 1,
          targetPathCount: 0,
          suggestedCheckCount: 0,
          allowedActions: ["run", "save", "revise", "reject"] as const,
          source: "explicit_plan_command" as const,
          stale: true,
        },
      }),
      planDecision,
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));

    expect(await screen.findByText("Stale plan draft")).toBeTruthy();
    expect(screen.getByText("Explicit /plan")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Review complete plan" }));
    expect(await screen.findByTestId("plan-workbench")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Run plan" }).hasAttribute("disabled")).toBe(true);
    expect(screen.getByRole("button", { name: "Save" }).hasAttribute("disabled")).toBe(true);
    expect(
      screen.getByText(/workspace changed since it was created\. Revise it to re-plan/),
    ).toBeTruthy();
    const reviseButton = screen.getByRole("button", { name: "Revise" });
    expect(reviseButton.hasAttribute("disabled")).toBe(false);
    await user.click(reviseButton);
    expect(planDecision).toHaveBeenCalledWith(
      workspace.id,
      "http-session-new",
      "plan-review-stale",
      `sha256:${"e".repeat(64)}`,
      "revise",
    );
    expect(await screen.findByText(/Plan revision started/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Reject" }).hasAttribute("disabled")).toBe(false);
  });

  it("submits a literal single-select option id without colliding with Other", async () => {
    const user = userEvent.setup();
    const requestHash = `sha256:${"7".repeat(64)}`;
    const userInputDecision = vi.fn(async (
      _workspaceId: string,
      sessionId: string,
      requestId: string,
      generation: number,
      expectedRequestHash: string,
    ) => ({
      commandId: "user-input-command-1",
      clientId: "desktop-test",
      sessionId,
      request: {
        identity: {
          sessionScopeId: sessionId,
          rootLogicalRunId: "root-run-1",
          sourceThreadId: "thread-1",
          requestId,
          generation,
          sourceBindingHash: `sha256:${"6".repeat(64)}`,
        },
        requestHash: expectedRequestHash,
        source: { kind: "agent" as const },
        purpose: "choice" as const,
        prompt: "Choose the execution mode",
        questions: [],
        allowedActions: ["submit" as const],
        requestedAtUnixMs: 1,
        status: "decision_accepted" as const,
        answerReceipt: {
          commandId: "user-input-command-1",
          decision: "submitted" as const,
          answerHash: `sha256:${"5".repeat(64)}`,
          answeredQuestionIds: ["mode"],
        },
      },
      continuationRunId: "continuation-1",
      replayed: false,
    }));
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      display: async () => ({
        schemaVersion: 1,
        requestScope: "http-session-new",
        throughSessionStreamSequence: "11",
        totalItems: "0",
        items: [],
        hasMore: false,
        gapFacts: [],
        userInput: {
          identity: {
            sessionScopeId: "http-session-new",
            rootLogicalRunId: "root-run-1",
            sourceThreadId: "thread-1",
            requestId: "request-1",
            generation: 1,
            sourceBindingHash: `sha256:${"6".repeat(64)}`,
          },
          requestHash,
          source: { kind: "agent" as const },
          purpose: "choice" as const,
          prompt: "Choose the execution mode",
          questions: [{
            id: "mode",
            header: "Mode",
            question: "Which mode should Sigil use?",
            required: true,
            field: {
              kind: "single_select" as const,
              options: [
                { id: "__other__", label: "Literal sentinel" },
                { id: "safe", label: "Safe" },
              ],
              allowOther: true,
            },
          }],
          allowedActions: ["submit" as const],
          requestedAtUnixMs: 1,
          status: "requested" as const,
        },
      }),
      userInputDecision,
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    const select = await screen.findByRole("combobox", { name: "Mode" });
    await user.selectOptions(select, "option:0");
    await user.click(screen.getByRole("button", { name: "Submit and continue" }));

    await waitFor(() => expect(userInputDecision).toHaveBeenCalledWith(
      workspace.id,
      "http-session-new",
      "request-1",
      1,
      requestHash,
      {
        kind: "submitted",
        answers: [{
          questionId: "mode",
          value: { kind: "single_select", optionId: "__other__" },
        }],
      },
      "manual",
    ));
  });

  it("stops a persistent terminal task with its exact run and generation binding", async () => {
    const user = userEvent.setup();
    let eventListener: ((event: TimelineEvent) => void) | undefined;
    const cancelTerminalTask = vi.fn(async (
      _workspaceId: string,
      sessionId: string,
      runId: string,
      taskId: string,
      expectedGeneration: number,
    ) => ({
      commandId: "terminal-cancel-1",
      clientId: "desktop-test",
      sessionId,
      runId,
      terminalTask: {
        taskId,
        generation: expectedGeneration + 1,
        status: "cancelled" as const,
        readiness: "ready" as const,
        readinessKind: "output_contains" as const,
        readyAtMs: 5,
        totalOutputBytes: 24,
        emittedAtMs: 7,
      },
      replayed: false,
    }));
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
      cancelTerminalTask,
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    await user.type(await readyComposer(), "Start the persistent development server");
    await user.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() => expect(eventListener).toBeDefined());
    act(() => eventListener?.({
      workspaceId: workspace.id,
      sessionId: "http-session-new",
      runId: "run-1",
      sequence: 4,
      runSequence: "4",
      replayable: true,
      kind: "terminal_lifecycle",
      itemId: "terminal-dev-server",
      status: "running",
      terminalTask: {
        taskId: "terminal-dev-server",
        executionBackend: "local_pty",
        sandboxProfile: "workspace_write",
        generation: 3,
        status: "running",
        readiness: "ready",
        readinessKind: "output_contains",
        readyAtMs: 3,
        totalOutputBytes: 12,
        emittedAtMs: 4,
      },
    }));

    expect(await screen.findByText("local pty")).toBeTruthy();
    expect(screen.getByText("workspace write")).toBeTruthy();

    await user.click(await screen.findByRole("button", { name: "Stop task" }));
    await waitFor(() => expect(cancelTerminalTask).toHaveBeenCalledWith(
      workspace.id,
      "http-session-new",
      "run-1",
      "terminal-dev-server",
      3,
    ));
    expect(await screen.findByText("Cancelled")).toBeTruthy();
  });

  it("keeps terminal lifecycle updates from an older run after a successor starts", async () => {
    const user = userEvent.setup();
    let eventListener: ((event: TimelineEvent) => void) | undefined;
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      continuity: async () => ({
        durableFrontier: { throughStreamSequence: 0 },
        retainedTerminalRuns: [],
        recoveryActions: [],
      }),
      subscribeRunEvents: async (listener) => {
        eventListener = listener;
        return () => undefined;
      },
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    await user.type(await readyComposer(), "Start a development server");
    await user.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() => expect(eventListener).toBeDefined());

    const event = (overrides: Partial<TimelineEvent>): TimelineEvent => ({
      workspaceId: workspace.id,
      sessionId: "http-session-new",
      runId: "run-1",
      sequence: 1,
      runSequence: "1",
      replayable: true,
      kind: "other",
      ...overrides,
    });
    act(() => {
      eventListener?.(event({
        kind: "terminal_lifecycle",
        itemId: "terminal-successor",
        terminalTask: {
          taskId: "terminal-successor",
          generation: 1,
          status: "running",
          readiness: "ready",
          readinessKind: "output_contains",
          readyAtMs: 1,
          totalOutputBytes: 8,
          emittedAtMs: 1,
        },
      }));
      eventListener?.(event({ kind: "run_finished", runSequence: "2", sequence: 2 }));
      eventListener?.(event({
        runId: "run-2",
        kind: "run_started",
        runSequence: "1",
        sequence: 3,
      }));
      eventListener?.(event({
        kind: "terminal_lifecycle",
        itemId: "terminal-successor",
        runSequence: "3",
        sequence: 4,
        terminalTask: {
          taskId: "terminal-successor",
          generation: 2,
          status: "exited",
          exitCode: 0,
          readiness: "ready",
          readinessKind: "output_contains",
          readyAtMs: 1,
          totalOutputBytes: 16,
          emittedAtMs: 3,
        },
      }));
    });

    await waitFor(() => expect(
      document.querySelector('[data-terminal-task-id="terminal-successor"]')
        ?.getAttribute("data-terminal-task-status"),
    ).toBe("exited"));
  });

  it("hydrates retained terminal owners from continuity and keeps stop control available", async () => {
    const user = userEvent.setup();
    const cancelTerminalTask = vi.fn(async (
      _workspaceId: string,
      sessionId: string,
      runId: string,
      taskId: string,
      expectedGeneration: number,
    ) => ({
      commandId: "terminal-hydrated-cancel",
      clientId: "desktop-test",
      sessionId,
      runId,
      terminalTask: {
        taskId,
        generation: expectedGeneration + 1,
        status: "cancelled" as const,
        readiness: "ready" as const,
        readinessKind: "output_contains" as const,
        readyAtMs: 4,
        totalOutputBytes: 12,
        emittedAtMs: 5,
      },
      replayed: false,
    }));
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      continuity: async () => ({
        durableFrontier: { throughStreamSequence: 7 },
        retainedTerminalRuns: [{
          runId: "run-retained-terminal",
          terminalTasks: [{
            taskId: "terminal-retained",
            generation: 7,
            status: "running",
            readiness: "ready",
            readinessKind: "output_contains",
            readyAtMs: 4,
            totalOutputBytes: 12,
            emittedAtMs: 4,
          }],
        }],
        recoveryActions: [],
      }),
      cancelTerminalTask,
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    await user.click(await screen.findByRole("button", { name: "Stop task" }));
    await waitFor(() => expect(cancelTerminalTask).toHaveBeenCalledWith(
      workspace.id,
      "http-session-new",
      "run-retained-terminal",
      "terminal-retained",
      7,
    ));
    expect(await screen.findByText("Cancelled")).toBeTruthy();
  });

  it("accepts the exact integration review and continues only after parent verification", async () => {
    const user = userEvent.setup();
    const review: TaskIntegrationReview = {
      schemaVersion: 1,
      request: {
        requestId: `integration-review-${"a".repeat(64)}`,
        taskId: "task-integration",
        planId: "integration-plan-1",
        planVersion: 4,
        previewDigest: `sha256:${"b".repeat(64)}`,
      },
      aggregateDiff: "diff --git a/src/lib.rs b/src/lib.rs\n+safe integration\n",
      aggregateDiffDigest: `sha256:${"c".repeat(64)}`,
      previewDigest: `sha256:${"b".repeat(64)}`,
      policyDigest: `sha256:${"d".repeat(64)}`,
      targetKind: "workspace_apply",
      lanes: [{
        laneId: "lane-1",
        candidateKind: "managed_ref",
        proposalCount: 2,
        verificationReceiptCount: 1,
      }],
      childVerificationReceiptCount: 2,
      laneVerificationReceiptCount: 1,
      conflictReasons: ["path_overlap"],
      verificationInvalidationCount: 1,
      parentVerificationPending: true,
    };
    const acceptTaskIntegration = vi.fn(async () => ({
      request: review.request,
      promotionStatus: "promoted" as const,
      parentVerdict: "passed" as const,
      canContinue: true,
    }));
    const continueTask = vi.fn(async (
      _workspaceId: string,
      sessionId: string,
      _taskId: string,
      permissionMode: PermissionMode,
    ) => ({
      id: "run-after-integration",
      sessionId,
      status: "running" as const,
      permissionMode,
      streamSequence: 0,
    }));
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      taskIntegrationReview: async () => review,
      acceptTaskIntegration,
      continueTask,
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    await user.click(await screen.findByRole("button", { name: "Review integration" }));
    expect(await screen.findByRole("dialog", { name: "Task integration review" })).toBeTruthy();
    expect(screen.getByText(/\+safe integration/)).toBeTruthy();
    expect(screen.getByText("lane-1")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Accept and continue" }));

    await waitFor(() => expect(acceptTaskIntegration).toHaveBeenCalledWith(
      workspace.id,
      "http-session-new",
      review.request,
    ));
    await waitFor(() => expect(continueTask).toHaveBeenCalledWith(
      workspace.id,
      "http-session-new",
      "task-integration",
      "manual",
      undefined,
    ));
  });

  it("does not continue an integration whose acceptance contradicts parent verification", async () => {
    const user = userEvent.setup();
    const review: TaskIntegrationReview = {
      schemaVersion: 1,
      request: {
        requestId: `integration-review-${"e".repeat(64)}`,
        taskId: "task-unverified",
        planId: "integration-plan-2",
        planVersion: 2,
        previewDigest: `sha256:${"f".repeat(64)}`,
      },
      aggregateDiff: "diff --git a/src/lib.rs b/src/lib.rs\n+unverified integration\n",
      aggregateDiffDigest: `sha256:${"1".repeat(64)}`,
      previewDigest: `sha256:${"f".repeat(64)}`,
      policyDigest: `sha256:${"2".repeat(64)}`,
      targetKind: "workspace_apply",
      lanes: [],
      childVerificationReceiptCount: 1,
      laneVerificationReceiptCount: 0,
      conflictReasons: [],
      verificationInvalidationCount: 0,
      parentVerificationPending: true,
    };
    const continueTask = vi.fn();
    const bridge = bridgeWith({
      bootstrap: async () => ({
        protocolVersion: 2,
        workspaces: [workspace],
        recentWorkspaces: [],
      }),
      taskIntegrationReview: async () => review,
      acceptTaskIntegration: async () => ({
        request: review.request,
        promotionStatus: "promoted",
        parentVerdict: "failed",
        canContinue: true,
      }),
      continueTask,
    });
    render(<App bridge={bridge} />);

    await screen.findByText("No matching conversation.");
    await user.click(screen.getByRole("button", { name: "New conversation" }));
    await user.click(await screen.findByRole("button", { name: "Review integration" }));
    await user.click(await screen.findByRole("button", { name: "Accept and continue" }));

    await waitFor(() => expect(screen.getByRole("button", { name: "Refresh review" })).toBeTruthy());
    expect(continueTask).not.toHaveBeenCalled();
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
