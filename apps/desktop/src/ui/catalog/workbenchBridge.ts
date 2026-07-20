import type { AppearanceSnapshot, ThemePreference } from "../../appearance/contract";
import type { DesktopBridge } from "../../bridge";
import type {
  CatalogPage,
  RunAttachment,
  RunContext,
  SessionSummary,
  TranscriptPage,
  VerificationSummary,
  WorkspaceSummary,
} from "../../types";

const workspace: WorkspaceSummary = {
  id: "catalog-workspace",
  displayName: "sigil",
  serverVersion: "catalog",
  state: "ready",
};

const session: SessionSummary = {
  id: "catalog-session-complete",
  label: "Review parser recovery and verification",
  runCount: 4,
  foregroundRunId: "catalog-run-active",
};

const catalog: CatalogPage = {
  workspaceId: workspace.id,
  generation: 1,
  reconciledAtUnixMs: 1_784_419_200_000,
  degradedSourceCount: 0,
  identityConflictCount: 0,
  truncatedSourceCount: 0,
  entries: [
    {
      sessionRef: "catalog-session-complete.jsonl",
      sessionId: session.id,
      sourceState: "ready",
      sourceModifiedAtUnixMs: 1_784_419_180_000,
      providerName: "deepseek",
      modelName: "deepseek-v4-flash",
      title: session.label,
      userMessageCount: 3,
      assistantMessageCount: 3,
      toolResultCount: 2,
      pinned: true,
    },
  ],
};

const transcript: TranscriptPage = {
  totalMessages: 4,
  messages: [
    {
      ordinal: 0,
      messageId: "catalog-user-1",
      role: "user",
      content: "检查 parser recovery。\n保留中文、emoji 🧭 与多行输入。",
      imageAttachmentCount: 0,
      truncated: false,
      originalContentBytes: 72,
    },
    {
      ordinal: 1,
      messageId: "catalog-assistant-1",
      role: "assistant",
      assistantKind: "final_answer",
      content: "### Current finding\n\nThe parser recovers safely, but the focused verification still fails.",
      imageAttachmentCount: 0,
      truncated: false,
      originalContentBytes: 93,
    },
    {
      ordinal: 2,
      messageId: "catalog-tool-1",
      role: "tool",
      toolName: "read_file",
      content: "Read src/parser.rs (184 lines)",
      imageAttachmentCount: 0,
      truncated: false,
      originalContentBytes: 31,
    },
    {
      ordinal: 3,
      messageId: "catalog-assistant-2",
      role: "assistant",
      assistantKind: "progress",
      content: "Running the exact parser check against the current workspace snapshot.",
      imageAttachmentCount: 0,
      truncated: false,
      originalContentBytes: 71,
    },
  ],
};

const attachment: RunAttachment = {
  run: {
    id: "catalog-run-active",
    sessionId: session.id,
    status: "running",
    approvalMode: "ask",
    streamSequence: 3,
  },
  events: [
    {
      workspaceId: workspace.id,
      sessionId: session.id,
      runId: "catalog-run-active",
      sequence: 1,
      replayable: true,
      kind: "run_started",
      text: "Repair the parser and rerun its focused verification.",
    },
    {
      workspaceId: workspace.id,
      sessionId: session.id,
      runId: "catalog-run-active",
      sequence: 2,
      replayable: true,
      kind: "tool_completed",
      itemId: "catalog-check",
      toolName: "shell",
      status: "failed",
      text: "parser::recovery_keeps_diagnostic_context failed",
    },
    {
      workspaceId: workspace.id,
      sessionId: session.id,
      runId: "catalog-run-active",
      sequence: 3,
      replayable: true,
      kind: "assistant_delta",
      text: "I found the failing assertion and am preparing a bounded fix.",
    },
  ],
  streamState: "live",
  hasGap: false,
};

const runContext: RunContext = {
  providerName: "deepseek",
  modelName: "deepseek-v4-flash",
  modelSelection: "fixed_for_session",
  defaultApprovalMode: "ask",
  availableApprovalModes: ["ask", "allow_readonly", "deny"],
  contextWindowTokens: 1_000_000,
  lastPromptTokens: 42_000,
  contextWindowSource: "provider",
};

const verification: VerificationSummary = {
  taskId: "catalog-task",
  stepId: "catalog-verify",
  scopeKind: "step",
  scopeId: "catalog-task:catalog-verify",
  verdict: "failed",
  status: "check failed",
  recommendedCheckSpecId: "cargo-test-parser",
  recommendationKind: "retry",
  recommendationReason: "The exact parser check failed for the current workspace snapshot.",
  action: {
    kind: "rerun",
    request: {
      taskId: "catalog-task",
      stepId: "catalog-verify",
      checkSpecId: "cargo-test-parser",
      checkSpecHash: "catalog-check-spec-hash",
      policyHash: "catalog-policy-hash",
      workspaceSnapshotId: "catalog-snapshot",
    },
  },
  evidence: {
    receiptId: "catalog-receipt",
    workspaceSnapshotId: "catalog-snapshot",
    changesetId: "catalog-changeset",
    commandEventId: "catalog-command",
    outputArtifactId: "catalog-output",
    failureSummary: "tests/parser.rs:42: expected diagnostic context",
  },
};

function appearance(preference: ThemePreference): AppearanceSnapshot {
  const resolvedTheme = preference === "light" ? "light" : "dark";
  return { preference, resolvedTheme };
}

export function createCatalogWorkbenchBridge(
  preference: ThemePreference,
): DesktopBridge {
  return {
    bootstrap: async () => ({
      protocolVersion: 1,
      workspaces: [workspace],
      recentWorkspaces: [],
      appearance: appearance(preference),
    }),
    setAppearance: async (next) => appearance(next),
    pickWorkspace: async () => ({ cancelled: true }),
    openRecentWorkspace: async () => workspace,
    closeWorkspace: async () => [],
    catalog: async () => catalog,
    createSession: async () => session,
    openSession: async () => session,
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
    transcript: async () => transcript,
    runContext: async () => runContext,
    startRun: async (_workspaceId, sessionId, _prompt, approvalMode) => ({
      id: "catalog-run-new",
      sessionId,
      status: "running",
      approvalMode,
      streamSequence: 0,
    }),
    attachRun: async () => attachment,
    cancelRun: async (_workspaceId, sessionId, runId) => ({
      id: runId,
      sessionId,
      status: "cancel_requested",
      approvalMode: "ask",
      streamSequence: 4,
    }),
    resolveApproval: async (_workspaceId, _sessionId, runId, request, approve) => ({
      runId,
      callId: request.callId,
      decision: approve ? "approved" : "denied",
    }),
    verification: async () => verification,
    rerunVerification: async () => ({ ...verification, verdict: "passed", status: "passed" }),
    subscribeRunEvents: async () => () => undefined,
    subscribeRunStreamStatus: async () => () => undefined,
    subscribeAppearance: async () => () => undefined,
  };
}
