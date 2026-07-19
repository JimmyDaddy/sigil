use std::fmt;

use serde::{Deserialize, Serialize};

/// Current command-envelope protocol accepted by `sigil serve`.
pub const DESKTOP_HTTP_PROTOCOL_VERSION: u16 = 1;

/// Request body for creating one process-local session handle.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(default, rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionCreateRequest {
    /// Optional user-visible label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Request body for reopening one durable catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionOpenRequest {
    /// Relative direct-child reference returned by the catalog.
    pub session_ref: String,
    /// Durable identity returned with the catalog entry.
    pub session_id: String,
    /// Optional process-local label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Process-local session snapshot returned by the authenticated server.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionSnapshot {
    /// Process-local session handle.
    pub id: String,
    /// Optional user-visible label.
    #[serde(default)]
    pub label: Option<String>,
    /// Runs registered under this handle.
    #[serde(default)]
    pub run_ids: Vec<String>,
    /// Durable session scope revalidated by the server.
    pub durable_session_scope_id: String,
    /// Server-private durable log path. Native-shell IPC must not project this field.
    pub session_log_path: String,
    /// Current foreground run, when leased.
    #[serde(default)]
    pub foreground_run_id: Option<String>,
}

impl fmt::Debug for DesktopSessionSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopSessionSnapshot")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("run_ids", &self.run_ids)
            .field("durable_session_scope_id", &self.durable_session_scope_id)
            .field("session_log_path", &"<redacted>")
            .field("foreground_run_id", &self.foreground_run_id)
            .finish()
    }
}

/// Response from listing process-local session handles.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionListResponse {
    /// Current handles in deterministic server order.
    pub sessions: Vec<DesktopSessionSnapshot>,
}

/// Provider-neutral role in the server-owned transcript projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopTranscriptRole {
    User,
    Assistant,
    Tool,
}

/// Assistant phase retained for correct transcript presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopTranscriptAssistantKind {
    ToolPreamble,
    Progress,
    ReasoningTrace,
    FinalAnswer,
}

/// One safe message from a bounded durable transcript page.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionTranscriptMessage {
    pub ordinal: u64,
    pub message_id: String,
    pub role: DesktopTranscriptRole,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub assistant_kind: Option<DesktopTranscriptAssistantKind>,
    #[serde(default)]
    pub tool_name: Option<String>,
    pub image_attachment_count: u64,
    pub truncated: bool,
    pub original_content_bytes: u64,
}

/// One chronological, backwards-pageable durable transcript page.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionTranscriptPage {
    pub session_scope_id: String,
    pub total_messages: u64,
    pub messages: Vec<DesktopSessionTranscriptMessage>,
    #[serde(default)]
    pub next_before: Option<u64>,
}

/// Bounded query for one durable transcript page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopTranscriptQuery {
    pub before: Option<u64>,
    pub limit: Option<u16>,
}

/// Historical catalog source classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopSessionCatalogState {
    /// The durable source can be reopened.
    Ready,
    /// The source exceeds the bounded catalog scan size.
    Oversized,
    /// The reconciliation scan budget was exhausted.
    ScanBudgetExceeded,
    /// The source predates the supported durable session format.
    UnsupportedLegacy,
    /// The durable source is malformed or inconsistent.
    Invalid,
}

/// One compact, body-free historical catalog row.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionCatalogEntry {
    pub workspace_id: String,
    pub session_ref: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub source_state: DesktopSessionCatalogState,
    pub source_bytes: u64,
    pub source_modified_at_unix_ms: u64,
    #[serde(default)]
    pub provider_name: Option<String>,
    #[serde(default)]
    pub model_name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    pub user_message_count: u64,
    pub assistant_message_count: u64,
    pub tool_result_count: u64,
    pub control_entry_count: u64,
    pub pinned: bool,
    pub indexed_at_unix_ms: u64,
}

/// Generation-consistent page of historical catalog rows.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionCatalogPage {
    pub workspace_id: String,
    pub generation: u64,
    pub reconciled_at_unix_ms: u64,
    pub degraded_source_count: u64,
    pub identity_conflict_count: u64,
    pub truncated_source_count: u64,
    pub entries: Vec<DesktopSessionCatalogEntry>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// Bounded filters for one catalog page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopCatalogQuery {
    pub limit: Option<u16>,
    pub cursor: Option<String>,
    pub query: Option<String>,
    pub provider: Option<String>,
    pub pinned: Option<bool>,
    pub state: Option<DesktopSessionCatalogState>,
}

/// Approval policy accepted by a run-start command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopRunApprovalMode {
    Deny,
    AllowReadonly,
    Ask,
}

/// Request payload for starting one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopRunStartRequest {
    pub prompt: String,
    pub approval_mode: DesktopRunApprovalMode,
}

/// Request payload for cooperative cancellation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(default, rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopRunCancelRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Public run lifecycle returned by the HTTP adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopRunStatus {
    Starting,
    Running,
    WaitingForApproval,
    CancelRequested,
    ExecutionUncertain,
    Finished,
    Failed,
    Cancelled,
    Interrupted,
}

impl DesktopRunStatus {
    /// Returns whether command routing has reached a terminal state.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Finished | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

/// Current adapter-owned run snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopRunSnapshot {
    pub id: String,
    pub session_id: String,
    pub status: DesktopRunStatus,
    pub approval_mode: DesktopRunApprovalMode,
    pub prompt_preview: String,
    #[serde(default)]
    pub pending_approval_call_ids: Vec<String>,
    pub stream_sequence: u64,
}

/// Versioned, idempotent command envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopCommandEnvelope<T> {
    pub protocol_version: u16,
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_stream_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub payload: T,
}

/// Receipt from starting a run.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopRunStartCommandReceipt {
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    #[serde(default)]
    pub correlation_id: Option<String>,
    pub run: DesktopRunSnapshot,
    pub replayed: bool,
}

/// Receipt from requesting cancellation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopRunCancelCommandReceipt {
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    #[serde(default)]
    pub expected_stream_sequence: Option<u64>,
    #[serde(default)]
    pub correlation_id: Option<String>,
    pub run: DesktopRunSnapshot,
    pub replayed: bool,
}

/// Guard material attached to a durable approval request event.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopPendingApproval {
    pub call_id: String,
    pub tool_name: String,
    pub approval_request_id: String,
    pub tool_call_hash: String,
    pub policy_version: String,
    pub expires_at_ms: u64,
}

/// Explicit user decision for one pending tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopApprovalDecision {
    Approve,
    Deny,
}

/// Persisted approval outcome returned in a command receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopApprovalRecordedDecision {
    Approved,
    Denied,
}

/// Server-owned approval decision record.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopApprovalDecisionRecord {
    pub run_id: String,
    pub call_id: String,
    pub decision: DesktopApprovalRecordedDecision,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Exact approval guard echoed back to the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopApprovalDecisionRequest {
    pub approval_request_id: String,
    pub tool_call_hash: String,
    pub policy_version: String,
    pub expires_at_ms: u64,
    pub decision: DesktopApprovalDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Receipt from resolving a pending approval.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopApprovalCommandReceipt {
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    pub run_id: String,
    pub call_id: String,
    #[serde(default)]
    pub expected_stream_sequence: Option<u64>,
    #[serde(default)]
    pub correlation_id: Option<String>,
    pub decision: DesktopApprovalDecisionRecord,
    pub replayed: bool,
}

/// Exact stale-safe binding for one recommended task verification check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopVerificationRerunRequest {
    pub task_id: String,
    pub step_id: String,
    pub check_spec_id: String,
    pub check_spec_hash: String,
    pub policy_hash: String,
    pub workspace_snapshot_id: String,
}

/// Verification evidence scope returned by the local server.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum DesktopVerificationScope {
    Run(String),
    Workspace(String),
    Task(String),
    Step(String),
    Agent(String),
    Changeset(String),
}

/// Shared verification readiness verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopVerificationVerdict {
    NotEvaluated,
    NotApplicable,
    Pending,
    Passed,
    Failed,
    Missing,
    Inconclusive,
    Stale,
    Skipped,
}

/// Latest durable check lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopVerificationCheckStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Inconclusive,
    Errored,
}

/// One exact product action; approval remains a review-only direction in this surface.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "request")]
pub enum DesktopVerificationAction {
    Rerun(DesktopVerificationRerunRequest),
    ReviewApproval { check_spec_id: String },
}

/// Stable reason category for one server-selected verification recommendation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopVerificationRecommendationKind {
    Run,
    RerunNonWriting,
    Retry,
    ReviewApproval,
}

/// Renderer-safe evidence links for verification inspection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopVerificationEvidence {
    pub check_run_id: Option<String>,
    pub check_spec_id: Option<String>,
    pub check_status: Option<DesktopVerificationCheckStatus>,
    pub receipt_id: Option<String>,
    pub workspace_snapshot_id: Option<String>,
    pub changeset_id: Option<String>,
    pub changeset_apply_event_id: Option<String>,
    pub command_event_id: Option<String>,
    pub output_artifact_id: Option<String>,
    pub failure_summary: Option<String>,
}

/// Shared verification recommendation and evidence view.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopVerificationView {
    pub task_id: String,
    pub step_id: String,
    pub scope: DesktopVerificationScope,
    pub verdict: DesktopVerificationVerdict,
    pub status: String,
    pub recommended_check_spec_id: Option<String>,
    pub recommendation_kind: Option<DesktopVerificationRecommendationKind>,
    pub recommendation_reason: Option<String>,
    pub action: Option<DesktopVerificationAction>,
    pub evidence: DesktopVerificationEvidence,
}

/// Receipt from one envelope-protected verification rerun.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopVerificationRerunCommandReceipt {
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    #[serde(default)]
    pub correlation_id: Option<String>,
    pub verification: DesktopVerificationView,
    pub replayed: bool,
}

/// Stable server error envelope. The native shell only projects the bounded code to the renderer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct DesktopErrorResponse {
    pub error: DesktopErrorBody,
}

/// Stable server error body.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct DesktopErrorBody {
    pub code: String,
    pub message: String,
}
