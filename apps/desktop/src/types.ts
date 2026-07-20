import type { AppearanceSnapshot } from "./appearance/contract";

export type ConnectionState = "ready" | "exited" | "crashed";

export interface WorkspaceSummary {
  id: string;
  displayName: string;
  serverVersion: string;
  state: ConnectionState;
}

export interface DesktopBootstrap {
  protocolVersion: 2;
  workspaces: WorkspaceSummary[];
  recentWorkspaces: RecentWorkspaceSummary[];
  appearance: AppearanceSnapshot;
}

export interface WorkspaceSelection {
  cancelled: boolean;
  workspace?: WorkspaceSummary;
}

export interface RecentWorkspaceSummary {
  id: string;
  displayName: string;
  isOpen: boolean;
}

export type CatalogSourceState =
  | "ready"
  | "oversized"
  | "scan_budget_exceeded"
  | "unsupported_legacy"
  | "invalid";

export interface CatalogRequest {
  limit?: number;
  cursor?: string;
  query?: string;
  provider?: string;
  pinned?: boolean;
  state?: CatalogSourceState;
}

export interface CatalogEntry {
  sessionRef: string;
  sessionId?: string;
  sourceState: CatalogSourceState;
  sourceBytes: number;
  sourceModifiedAtUnixMs: number;
  providerName?: string;
  modelName?: string;
  title?: string;
  userMessageCount: number;
  assistantMessageCount: number;
  toolResultCount: number;
  pinned: boolean;
}

export interface CatalogPage {
  workspaceId: string;
  generation: number;
  reconciledAtUnixMs: number;
  degradedSourceCount: number;
  identityConflictCount: number;
  truncatedSourceCount: number;
  entries: CatalogEntry[];
  nextCursor?: string;
}

export interface SessionSummary {
  id: string;
  label?: string;
  runCount: number;
  foregroundRunId?: string;
}

export interface SessionOpenInput {
  sessionRef: string;
  sessionId: string;
  label?: string;
}

export interface SessionRenameInput {
  sessionRef: string;
  sessionId: string;
  displayName: string;
}

export interface SessionDeleteInput {
  sessionRef: string;
  sessionId: string;
}

export interface SessionQuarantineInput {
  sessionRef: string;
  sourceBytes: number;
  sourceModifiedAtUnixMs: number;
}

export interface SessionMutationSummary {
  sessionRef: string;
  sessionId: string;
  projectionGeneration?: number;
}

export interface SessionQuarantineSummary {
  sessionRef: string;
  quarantineName: string;
  projectionGeneration?: number;
}

export type TranscriptRole = "user" | "assistant" | "tool";

export type TranscriptAssistantKind =
  | "tool_preamble"
  | "progress"
  | "reasoning_trace"
  | "final_answer";

export interface TranscriptMessage {
  ordinal: number;
  messageId: string;
  role: TranscriptRole;
  content?: string;
  assistantKind?: TranscriptAssistantKind;
  toolName?: string;
  imageAttachmentCount: number;
  truncated: boolean;
  originalContentBytes: number;
}

export interface TranscriptPage {
  totalMessages: number;
  messages: TranscriptMessage[];
  nextBefore?: number;
}

export interface TranscriptRequest {
  before?: number;
  limit?: number;
}

export type RunStatus =
  | "starting"
  | "running"
  | "waiting_for_approval"
  | "cancel_requested"
  | "execution_uncertain"
  | "finished"
  | "failed"
  | "cancelled"
  | "interrupted";

export type PermissionMode = "read-only" | "manual" | "auto-edit" | "danger-full-access";
export type ReasoningEffort = "low" | "medium" | "high" | "max";
export type ApplicationClientAction =
  | "new_session"
  | "focus_effort"
  | "focus_model"
  | "open_session_picker"
  | "open_agent_workbench";

export interface CommandCatalogEntry {
  canonical: string;
  aliases: string[];
  label: string;
  description: string;
  argumentHint?: string;
  completesWithSpace: boolean;
  clientAction?: ApplicationClientAction;
  available: boolean;
  unavailableReason?: string;
}

export interface SkillBinding {
  skillId: string;
  skillSha256: string;
  indexFingerprint: string;
}

export interface SkillCatalogEntry {
  id: string;
  invocationToken: string;
  name: string;
  description: string;
  source: string;
  runMode: string;
  trust: string;
  available: boolean;
  unavailableReason?: string;
  binding?: SkillBinding;
}

export interface AgentCatalogEntry {
  id: string;
  invocationToken: string;
  description: string;
  source: string;
  kind: string;
  trust: string;
  enabled: boolean;
  userInvocable: boolean;
  available: boolean;
  unavailableReason?: string;
  snapshotId?: string;
}

export interface ExtensionCatalog {
  commands: CommandCatalogEntry[];
  skills: SkillCatalogEntry[];
  agents: AgentCatalogEntry[];
}

export interface RunContext {
  providerName: string;
  modelName: string;
  availableModels: string[];
  modelSelection: "fixed_for_session";
  defaultPermissionMode: PermissionMode;
  availablePermissionModes: PermissionMode[];
  availableReasoningEfforts: ReasoningEffort[];
  defaultReasoningEffort?: ReasoningEffort;
  reasoningEffortBinding?: string;
  contextWindowTokens?: number;
  lastPromptTokens?: number;
  contextWindowSource: "provider" | "config" | "unavailable";
  extensionCatalog: ExtensionCatalog;
}

export interface RunSummary {
  id: string;
  sessionId: string;
  status: RunStatus;
  permissionMode: PermissionMode;
  reasoningEffort?: ReasoningEffort;
  streamSequence: number;
}

export interface RunAttachment {
  run: RunSummary;
  events: TimelineEvent[];
  streamState: RunStreamState;
  streamMessage?: string;
  hasGap: boolean;
}

export interface ApprovalDecisionSummary {
  runId: string;
  callId: string;
  decision: "approved" | "denied";
}

export interface VerificationRerunBinding {
  taskId: string;
  stepId: string;
  checkSpecId: string;
  checkSpecHash: string;
  policyHash: string;
  workspaceSnapshotId: string;
}

export type VerificationAction =
  | { kind: "rerun"; request: VerificationRerunBinding }
  | { kind: "review_approval"; checkSpecId: string };

export interface VerificationEvidence {
  checkRunId?: string;
  checkSpecId?: string;
  checkStatus?: "queued" | "running" | "succeeded" | "failed" | "skipped" | "inconclusive" | "errored";
  receiptId?: string;
  workspaceSnapshotId?: string;
  changesetId?: string;
  changesetApplyEventId?: string;
  commandEventId?: string;
  outputArtifactId?: string;
  failureSummary?: string;
}

export interface VerificationSummary {
  taskId: string;
  stepId: string;
  scopeKind: "run" | "workspace" | "task" | "step" | "agent" | "changeset";
  scopeId: string;
  verdict: "not_evaluated" | "not_applicable" | "pending" | "passed" | "failed" | "missing" | "inconclusive" | "stale" | "skipped";
  status: string;
  recommendedCheckSpecId?: string;
  recommendationKind?: "run" | "rerun_non_writing" | "retry" | "review_approval";
  recommendationReason?: string;
  action?: VerificationAction;
  evidence: VerificationEvidence;
}

export type TimelineEventKind =
  | "run_started"
  | "assistant_delta"
  | "reasoning_delta"
  | "assistant_message"
  | "tool_started"
  | "tool_completed"
  | "tool_progress"
  | "tool_result"
  | "approval_requested"
  | "approval_resolved"
  | "notice"
  | "usage"
  | "control"
  | "run_finished"
  | "run_failed"
  | "run_cancelled"
  | "other";

export interface TimelineApproval {
  callId: string;
  toolName: string;
  approvalRequestId: string;
  toolCallHash: string;
  policyVersion: string;
  expiresAtMs: number;
  operation?: string;
  risk?: string;
  snapshotRequired: boolean;
  previewTitle?: string;
  previewSummary?: string;
  previewBody?: string;
}

export interface TimelineEvent {
  workspaceId: string;
  sessionId: string;
  runId: string;
  sequence: number;
  replayable: boolean;
  replayId?: string;
  kind: TimelineEventKind;
  text?: string;
  itemId?: string;
  toolName?: string;
  status?: string;
  approval?: TimelineApproval;
}

export type RunStreamState =
  | "connecting"
  | "live"
  | "reconnecting"
  | "terminal"
  | "error";

export interface RunStreamStatus {
  workspaceId: string;
  sessionId: string;
  runId: string;
  state: RunStreamState;
  message?: string;
}
