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

export type SupportStatus = "ok" | "warn" | "error";

export interface SupportDoctorReport {
  generatedAtUnixMs: number;
  version: string;
  commit: string;
  target: string;
  profile: string;
  environment: {
    os: string;
    architecture: string;
    terminalFamily: string;
  };
  summary: {
    overallStatus: SupportStatus;
    ok: number;
    warn: number;
    error: number;
  };
  checks: Array<{
    status: SupportStatus;
    name: string;
    summary: string;
    remediation?: string;
  }>;
  privacy: {
    included: string[];
    excluded: string[];
    reviewBeforeSharing: boolean;
  };
}

export interface SupportSaveSummary {
  cancelled: boolean;
  fileName?: string;
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

export interface ForegroundRunOwner {
  runId: string;
  ownerRevision: string;
}

export type ContinuityRecoveryAction =
  | "retry_current"
  | "open_another_workspace"
  | "open_diagnostics"
  | "show_details"
  | "continue_read_only";

export interface ConversationContinuity {
  durableFrontier: {
    throughStreamSequence: number;
  };
  foregroundOwner?: ForegroundRunOwner;
  recoveryActions: ContinuityRecoveryAction[];
}

export type ConversationQueueItemKind =
  | "chat"
  | "plan_prompt"
  | "agent_mention"
  | "agent_message"
  | "unknown";

export type ConversationQueueItemStatus =
  | "queued"
  | "dispatching"
  | "delivered"
  | "rejected"
  | "cancelled"
  | "stale"
  | "unknown";

export type ConversationQueuePromptMaterial =
  | "persisted_safe"
  | "available_process_local"
  | "requires_reentry";

export type ConversationQueueBlockedReason =
  | "queue_paused"
  | "requires_reentry"
  | "foreground_run_active"
  | "waiting_for_terminal_frontier"
  | "foreground_owner_lost"
  | "permission_required"
  | "conflict"
  | "stale"
  | "terminal"
  | "unsupported_target"
  | "material_unavailable";

export interface ConversationQueueItem {
  entryId: string;
  order: number;
  kind: ConversationQueueItemKind;
  status: ConversationQueueItemStatus;
  promptPreview: string;
  promptPreviewTruncated: boolean;
  promptMaterial: ConversationQueuePromptMaterial;
  dispatchable: boolean;
  blockedReason?: ConversationQueueBlockedReason;
  createdAtMs?: number;
  updatedAtMs?: number;
}

export interface ConversationQueueView {
  schemaVersion: number;
  sessionId: string;
  generation: string;
  paused: boolean;
  totalItems: number;
  items: ConversationQueueItem[];
  truncated: boolean;
  nextDispatchableEntryId?: string;
}

export type ConversationQueueCommandAction =
  | {
      action: "enqueue";
      prompt: string;
      kind: ConversationQueueItemKind;
      reasoningEffort?: ReasoningEffort;
    }
  | {
      action: "edit";
      entryId: string;
      prompt: string;
      reasoningEffort?: ReasoningEffort;
    }
  | { action: "remove"; entryId: string }
  | { action: "reorder"; entryId: string; afterEntryId?: string }
  | { action: "pause" }
  | { action: "resume" }
  | {
      action: "interrupt_and_run_next";
      foregroundRunId: string;
      foregroundOwnerRevision: string;
    };

export interface ConversationQueueCommandInput {
  sessionId: string;
  expectedGeneration: string;
  action: ConversationQueueCommandAction;
}

export type ConversationQueueCommandActionKind = ConversationQueueCommandAction["action"];

export interface ConversationQueueCommandReceipt {
  commandId: string;
  clientId: string;
  sessionId: string;
  action: ConversationQueueCommandActionKind;
  expectedGeneration: string;
  generation: string;
  interruptOwner?: ForegroundRunOwner;
  queue: ConversationQueueView;
  correlationId?: string;
  replayed: boolean;
}

export type CheckpointRestoreKind = "restore_content" | "remove_created_file";
export type CheckpointFileAvailability = "restorable" | "sensitive" | "unsupported" | "unavailable";

export interface CheckpointFileView {
  path: string;
  restoreKind: CheckpointRestoreKind;
  availability: CheckpointFileAvailability;
}

export interface CheckpointView {
  checkpointId: string;
  checkpointDigest: string;
  turnIndex: number;
  prompt?: string;
  files: CheckpointFileView[];
  unknownMutationCount: number;
  fullyRestorable: boolean;
}

export interface ConversationForkPointView {
  sourceTurnIndex: number;
  sourceTurnDigest: string;
  sourceBoundaryStreamSequence: number;
  sourceFinalizedStreamSequence: number;
}

export interface ConversationRecoveryView {
  checkpoints: CheckpointView[];
  forkPoints: ConversationForkPointView[];
  throughStreamSequence: number;
}

export interface CompactionEconomics {
  beforeInputTokens: number;
  targetInputTokens: number;
  contextWindowTokens: number;
  outputTokens: number;
  safetyBufferTokens: number;
  savingsTokens: number;
  savingsRatioPpm: number;
  minimumSavingsTokens: number;
  minimumSavingsRatioPpm: number;
}

export type CompactionAdmission =
  | { kind: "ready"; economics: CompactionEconomics }
  | {
      kind: "no_foldable_history";
      durableMessageCount: number;
      configuredTailMessageCount: number;
    }
  | { kind: "unavailable"; reason: string };

export interface CompactionReview {
  previewId?: string;
  foldedEventCount: number;
  retainedEventCount: number;
  admission: CompactionAdmission;
}

export interface CheckpointRestorePreviewInput {
  sessionId: string;
  checkpointId: string;
  checkpointDigest: string;
}

export type CheckpointRestoreConflictReason =
  | "workspace_mismatch"
  | "current_hash_mismatch"
  | "artifact_unavailable"
  | "sensitive_snapshot"
  | "unsupported_snapshot"
  | "invalid_binding";

export interface CheckpointRestorePreviewFile {
  path: string;
  restoreKind: CheckpointRestoreKind;
  expectedCurrentHash?: string;
  actualCurrentHash?: string;
  conflictReason?: CheckpointRestoreConflictReason;
}

export interface CheckpointReverseDiff {
  path: string;
  diff: string;
  truncated: boolean;
  originalLineCount: number;
}

export interface CheckpointRestoreReview {
  checkpointId: string;
  checkpointDigest: string;
  files: CheckpointRestorePreviewFile[];
  reverseDiffs: CheckpointReverseDiff[];
  unknownMutationCount: number;
  ready: boolean;
}

export type ConversationRecoveryAction =
  | { kind: "apply_compaction"; previewId: string }
  | { kind: "restore_checkpoint"; checkpointId: string; checkpointDigest: string }
  | { kind: "fork_conversation"; sourceTurnDigest: string };

export interface ConversationRecoveryCommandInput {
  sessionId: string;
  action: ConversationRecoveryAction;
}

export interface CheckpointRestoreReceipt {
  checkpointId: string;
  batchId: string;
  restoredFileCount: number;
  verificationStale: boolean;
}

export interface CompactionReceipt {
  compactionId: string;
  attemptId: string;
  taskMemoryId: string;
  foldedEventCount: number;
  toolOutputProjectionRecorded: boolean;
}

export interface ConversationForkReceipt {
  sessionRef: string;
  sessionId: string;
  copiedMessageCount: number;
  copiedExternalProvenanceCount: number;
}

export interface ConversationRecoveryCommandReceipt {
  commandId: string;
  clientId: string;
  sessionId: string;
  action: ConversationRecoveryAction["kind"];
  compaction?: CompactionReceipt;
  restore?: CheckpointRestoreReceipt;
  fork?: ConversationForkReceipt;
  recovery: ConversationRecoveryView;
  correlationId?: string;
  replayed: boolean;
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

export interface SessionInvalidSourceDeleteInput {
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

export interface SessionInvalidSourceDeleteSummary {
  sessionRef: string;
  projectionGeneration?: number;
}

export type SessionCatalogBatchAction =
  | "delete_sessions"
  | "quarantine_invalid_sources"
  | "delete_invalid_sources";

export interface SessionCatalogBatchItem {
  sessionRef: string;
  sessionId?: string;
  sourceBytes?: number;
  sourceModifiedAtUnixMs?: number;
}

export interface SessionCatalogBatchPlanInput {
  action: SessionCatalogBatchAction;
  items: SessionCatalogBatchItem[];
}

export interface SessionCatalogBatchExecuteInput extends SessionCatalogBatchPlanInput {
  planId: string;
}

export interface SessionCatalogBatchPlanItem {
  sessionRef: string;
  status: "executable" | "blocked";
  reason?: string;
}

export interface SessionCatalogBatchPlan {
  planId: string;
  action: SessionCatalogBatchAction;
  generation: number;
  total: number;
  executable: number;
  blocked: number;
  items: SessionCatalogBatchPlanItem[];
}

export interface SessionCatalogBatchReceiptItem {
  sessionRef: string;
  outcome: "completed" | "failed" | "skipped";
  reason?: string;
  operationId?: string;
  quarantineName?: string;
  projectionGeneration?: number;
}

export interface SessionCatalogBatchReceipt {
  planId: string;
  action: SessionCatalogBatchAction;
  total: number;
  completed: number;
  failed: number;
  skipped: number;
  items: SessionCatalogBatchReceiptItem[];
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

export type ConversationDisplayItemKind =
  | "user_message"
  | "reasoning"
  | "assistant_message"
  | "tool"
  | "approval"
  | "checkpoint"
  | "notice"
  | "terminal";

export type ConversationDisplaySource =
  | "durable_transcript"
  | "durable_run_event"
  | "live_transient";

export type ConversationDisplayStatus =
  | "recorded"
  | "requested"
  | "waiting_for_approval"
  | "approved"
  | "denied"
  | "completed"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "interrupted"
  | "blocked";

export type ConversationDisplayContent =
  | {
      type: "message";
      role: "user" | "assistant";
      text?: string;
      assistantPhase?: "tool_preamble" | "progress" | "final_answer";
      imageAttachmentCount: number;
      truncated: boolean;
      originalContentBytes: number;
    }
  | {
      type: "reasoning";
      text: string;
      truncated: boolean;
      originalContentBytes: number;
    }
  | {
      type: "tool";
      callId?: string;
      toolName?: string;
      output?: string;
      truncated: boolean;
      originalContentBytes: number;
    }
  | {
      type: "approval";
      callId: string;
      toolName: string;
      decision?: "approved" | "approved_for_session" | "denied";
    }
  | {
      type: "checkpoint";
      outcome: "restored" | "conflict";
      checkpointId?: string;
      conflictReason?:
        | "workspace_mismatch"
        | "current_hash_mismatch"
        | "artifact_unavailable"
        | "sensitive_snapshot"
        | "unsupported_snapshot"
        | "invalid_binding";
    }
  | {
      type: "notice";
      text: string;
      truncated: boolean;
      originalContentBytes: number;
    }
  | {
      type: "terminal";
      finalMessageId?: string;
      safeSummary?: string;
      summaryTruncated: boolean;
    };

export interface ConversationDisplayItem {
  schemaVersion: number;
  displayId: string;
  displayOrder: {
    sessionStreamSequence: string;
    subindex: number;
  };
  sourceEventId: string;
  kind: ConversationDisplayItemKind;
  source: ConversationDisplaySource;
  runId?: string;
  runSequence?: string;
  status: ConversationDisplayStatus;
  content: ConversationDisplayContent;
  reconciles?: string[];
}

export interface ConversationDisplayPage {
  schemaVersion: number;
  requestScope: string;
  throughSessionStreamSequence: string;
  terminalFrontier?: {
    runId: string;
    sessionStreamSequence: string;
    status: ConversationDisplayStatus;
  };
  totalItems: string;
  items: ConversationDisplayItem[];
  nextCursor?: string;
  hasMore: boolean;
  gapFacts: Array<{
    kind: string;
    afterSessionStreamSequence: string;
  }>;
  liveProvisionalAnchor?: {
    durableFrontier: string;
    runId: string;
    runSequence: string;
  };
}

export interface ConversationDisplayRequest {
  cursor?: string;
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
  | "preview_compaction"
  | "new_session"
  | "focus_effort"
  | "focus_model"
  | "open_session_picker"
  | "open_agent_workbench"
  | "open_settings"
  | "open_support";

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
  binding?: AgentBinding;
}

export interface AgentBinding {
  profileId: string;
  snapshotId: string;
}

export type AgentActivityStatus =
  | "started"
  | "running"
  | "blocked"
  | "completed"
  | "failed"
  | "cancelled"
  | "interrupted"
  | "unavailable"
  | "unknown";

export type AgentHandoffStatus =
  | "pending"
  | "result_ready"
  | "result_read"
  | "returned"
  | "unavailable";

export interface AgentActivityItem {
  threadId: string;
  profileId?: string;
  displayName?: string;
  objective: string;
  status: AgentActivityStatus;
  reason?: string;
  handoffStatus: AgentHandoffStatus;
  resultSummary?: string;
  resultSummaryTruncated: boolean;
  usage?: {
    inputTokens: number;
    outputTokens: number;
    totalTokens: number;
    cachedTokens?: number;
  };
}

export interface AgentActivitySummary {
  totalAgents: number;
  activeAgents: number;
  terminalAgents: number;
  items: AgentActivityItem[];
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
  modelOptions: ModelOption[];
  modelSelection: "per_run";
  modelSelectionBinding: string;
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

export interface ModelOption {
  modelName: string;
  availableReasoningEfforts: ReasoningEffort[];
  defaultReasoningEffort?: ReasoningEffort;
  reasoningEffortBinding?: string;
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

export interface RunAttachInput {
  sessionId: string;
  runId: string;
  ownerRevision: string;
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
  sessionGrantAvailable?: boolean;
  toolInput?: string;
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
  /** Exact decimal form emitted by the native bridge and used for reconciliation ordering. */
  runSequence: string;
  replayable: boolean;
  replayId?: string;
  provisionalId?: string;
  kind: TimelineEventKind;
  text?: string;
  itemId?: string;
  toolName?: string;
  status?: string;
  assistantKind?: "tool_preamble" | "progress" | "reasoning_trace" | "final_answer";
  toolInput?: string;
  approval?: TimelineApproval;
}

export type ApprovalAction = "approve_once" | "approve_session" | "deny";

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
