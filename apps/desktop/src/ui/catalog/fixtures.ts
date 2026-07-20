import type { ToolView } from "../../ToolCard";
import type {
  CatalogEntry,
  RunContext,
  RunStreamState,
  TimelineApproval,
  VerificationSummary,
} from "../../types";

export const UI_CATALOG_MARKER = "sigil-desktop-dev-ui-catalog";

export type CatalogTheme = "system" | "light" | "dark";
export type CatalogViewport = 1280 | 1024 | 900 | 899 | 760 | 320;
export type CatalogContrast = "normal" | "forced-colors";
export type CatalogMotion = "full" | "reduced";

export interface CatalogFixture {
  readonly id: string;
  readonly description: string;
  readonly themes: readonly CatalogTheme[];
  readonly viewports: readonly CatalogViewport[];
  readonly sessions?: readonly CatalogEntry[];
  readonly minimumFullyVisibleRows1280x720?: number;
  readonly degradedCounts?: {
    readonly unavailable: number;
    readonly changed: number;
    readonly truncated: number;
  };
  readonly tool?: ToolView;
  readonly approval?: TimelineApproval;
  readonly verification?: VerificationSummary;
  readonly diff?: string;
  readonly streamState?: RunStreamState;
  readonly attachmentGap?: boolean;
  readonly longCopy?: string;
  readonly fullWorkbench?: boolean;
  readonly composer?: {
    readonly active: boolean;
    readonly context: RunContext;
  };
  readonly contrastModes: readonly CatalogContrast[];
  readonly motionModes: readonly CatalogMotion[];
  readonly zoomFactors: readonly (1 | 2)[];
}

const allThemes = ["system", "light", "dark"] as const;
const allViewports = [1280, 1024, 900, 899, 760, 320] as const;
const allContrastModes = ["normal", "forced-colors"] as const;
const allMotionModes = ["full", "reduced"] as const;
const allZoomFactors = [1, 2] as const;

const environment = {
  themes: allThemes,
  viewports: allViewports,
  contrastModes: allContrastModes,
  motionModes: allMotionModes,
  zoomFactors: allZoomFactors,
} as const;

function sessionEntries(count: number): CatalogEntry[] {
  return Array.from({ length: count }, (_, index) => ({
    sessionRef: `catalog-session-${index + 1}.jsonl`,
    sessionId: `catalog-session-${index + 1}`,
    sourceState: "ready",
    sourceModifiedAtUnixMs: 1_784_419_200_000 - index * 60_000,
    providerName: index % 2 === 0 ? "deepseek" : "openai",
    modelName: index % 2 === 0 ? "deepseek-chat" : "gpt-5",
    title: index % 3 === 0
      ? `Investigate a long-running workspace regression ${index + 1}`
      : `检查桌面端会话密度与键盘导航 ${index + 1}`,
    userMessageCount: index + 1,
    assistantMessageCount: index,
    toolResultCount: index % 4,
    pinned: index < 2,
  }));
}

export const catalogFixtures: readonly CatalogFixture[] = [
  {
    id: "no-workspace",
    description: "No workspace selected",
    ...environment,
  },
  {
    id: "empty-catalog",
    description: "Ready workspace with no conversations",
    ...environment,
    sessions: [],
  },
  {
    id: "session-catalog-30",
    description: "Thirty sessions with long English and Chinese titles",
    ...environment,
    sessions: sessionEntries(30),
    minimumFullyVisibleRows1280x720: 8,
  },
  {
    id: "session-catalog-100",
    description: "One hundred sessions for bounded rail scrolling",
    ...environment,
    sessions: sessionEntries(100),
  },
  {
    id: "degraded-catalog",
    description: "Catalog with unavailable, changed, and truncated sources",
    ...environment,
    sessions: [
      {
        sessionRef: "legacy.jsonl",
        sourceState: "unsupported_legacy",
        sourceModifiedAtUnixMs: 1_784_419_100_000,
        title: "Unavailable legacy source",
        userMessageCount: 0,
        assistantMessageCount: 0,
        toolResultCount: 0,
        pinned: false,
      },
      {
        sessionRef: "oversized.jsonl",
        sourceState: "oversized",
        sourceModifiedAtUnixMs: 1_784_419_000_000,
        title: "Oversized conversation",
        userMessageCount: 120,
        assistantMessageCount: 120,
        toolResultCount: 40,
        pinned: false,
      },
    ],
    degradedCounts: { unavailable: 2, changed: 1, truncated: 1 },
  },
  {
    id: "workbench-complete",
    description: "Complete application workbench with an active run and failed verification",
    ...environment,
    fullWorkbench: true,
  },
  {
    id: "running-tool-approval",
    description: "Active run with a tool result and high-risk approval",
    ...environment,
    streamState: "live",
    tool: {
      key: "catalog-tool-running",
      toolName: "write_file",
      text: "Updated src/parser.rs",
      status: "succeeded",
      duration: "184 ms",
      risk: "high",
    },
    approval: {
      callId: "catalog-call-high-risk",
      toolName: "write_file",
      approvalRequestId: "catalog-approval-high-risk",
      toolCallHash: "catalog-tool-hash",
      policyVersion: "catalog-policy-v1",
      expiresAtMs: 4_102_444_800_000,
      operation: "edit_file",
      risk: "high",
      snapshotRequired: true,
      previewTitle: "Replace parser implementation",
      previewSummary: "Review the exact protected file mutation.",
      previewBody: "--- a/src/parser.rs\n+++ b/src/parser.rs\n@@ -1 +1 @@\n-old\n+new",
    },
  },
  {
    id: "reconnect-gap",
    description: "Reconnecting active run with a retained-event gap",
    ...environment,
    streamState: "reconnecting",
    attachmentGap: true,
  },
  {
    id: "coding-composer",
    description: "Sticky coding composer with model, approval mode, context, and send control",
    ...environment,
    composer: {
      active: false,
      context: {
        providerName: "deepseek",
        modelName: "deepseek-v4-flash",
        modelSelection: "fixed_for_session",
        defaultApprovalMode: "ask",
        availableApprovalModes: ["ask", "allow_readonly", "deny"],
        contextWindowTokens: 1_000_000,
        lastPromptTokens: 42_000,
        contextWindowSource: "provider",
      },
    },
  },
  {
    id: "tool-error-raw-details",
    description: "Failed write tool with a human summary and collapsed transport details",
    ...environment,
    tool: {
      key: "catalog-tool-denied",
      toolName: "write_file",
      text: JSON.stringify({
        content: "Tool execution was denied in Sigil Desktop.",
        error: { kind: "approval_denied", message: "Denied in Sigil Desktop", retriable: false },
        meta: { details: { call: { path: "src/parser.rs", summary: "path=src/parser.rs" } } },
        status: "error",
      }),
      status: "error",
    },
  },
  {
    id: "verification-failed-diff",
    description: "Failed verification with diff and evidence context",
    ...environment,
    verification: {
      taskId: "catalog-task",
      stepId: "catalog-verify",
      scopeKind: "step",
      scopeId: "catalog-task:catalog-verify",
      verdict: "failed",
      status: "check failed",
      recommendedCheckSpecId: "cargo-test",
      recommendationKind: "retry",
      recommendationReason: "The latest result failed for the current workspace snapshot.",
      evidence: {
        receiptId: "catalog-receipt",
        workspaceSnapshotId: "catalog-snapshot",
        changesetId: "catalog-changeset",
        commandEventId: "catalog-command",
        outputArtifactId: "catalog-output",
        failureSummary: "tests/parser.rs:42: expected valid syntax",
      },
    },
    diff: "--- a/src/parser.rs\n+++ b/src/parser.rs\n@@ -1 +1 @@\n-old\n+new",
  },
  {
    id: "long-copy",
    description: "Long English and Chinese content without horizontal document overflow",
    ...environment,
    longCopy: "Investigate a deeply nested workspace regression without truncating its primary action. 检查一个包含很长中文说明的桌面端会话，确保标题、工具输出和审批说明不会导致页面横向滚动。",
  },
  {
    id: "missing-optional-metadata",
    description: "Tool result with no duration, risk, provider, or model metadata",
    ...environment,
    tool: {
      key: "catalog-tool-minimal",
      toolName: "shell",
      text: "cargo test passed",
    },
  },
];
