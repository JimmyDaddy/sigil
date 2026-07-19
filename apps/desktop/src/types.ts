export type ConnectionState = "ready" | "exited" | "crashed";

export interface WorkspaceSummary {
  id: string;
  displayName: string;
  serverVersion: string;
  state: ConnectionState;
}

export interface DesktopBootstrap {
  protocolVersion: 1;
  workspaces: WorkspaceSummary[];
  recentWorkspaces: RecentWorkspaceSummary[];
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

export interface RunSummary {
  id: string;
  sessionId: string;
  status: RunStatus;
  streamSequence: number;
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
