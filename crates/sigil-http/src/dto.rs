use std::fmt;

use serde::{Deserialize, Serialize};
use sigil_kernel::ToolApprovalUserDecision;

/// Request body for creating one HTTP adapter session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct HttpSessionCreateRequest {
    /// Optional user-facing label for clients that manage multiple sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Public snapshot returned by session create/get endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpSessionSnapshot {
    /// HTTP adapter session id.
    pub id: String,
    /// Optional user-facing label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Runs that were registered under this HTTP session.
    #[serde(default)]
    pub run_ids: Vec<String>,
    /// Durable V2 session scope bound to this process-local adapter session.
    pub durable_session_scope_id: String,
    /// Durable JSONL session path selected by the runtime adapter.
    pub session_log_path: String,
    /// Current foreground run, when this session is leased for execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_run_id: Option<String>,
}

/// Runtime-owned durable binding for one process-local HTTP adapter session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpSessionBinding {
    /// Durable V2 session scope id derived from the canonical session path.
    pub session_scope_id: String,
    /// Canonical durable JSONL session path exposed to the local authenticated adapter.
    pub session_log_path: String,
}

/// Request body for starting one run inside an HTTP adapter session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct HttpRunStartRequest {
    /// User prompt for the run.
    pub prompt: String,
    /// Explicit HTTP approval policy for the run.
    ///
    /// The HTTP adapter intentionally exposes `allow_readonly` instead of a broad `allow`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_mode: Option<HttpRunApprovalMode>,
}

/// Request body for cancelling one run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct HttpRunCancelRequest {
    /// Optional user-facing reason for diagnostics and future audit surfaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Approval policy accepted by the HTTP run start endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpRunApprovalMode {
    /// Deny tool calls that need approval.
    Deny,
    /// Allow read-only work while keeping mutating operations gated by policy.
    AllowReadonly,
    /// Require an explicit approval endpoint decision for gated tool calls.
    Ask,
}

impl HttpRunApprovalMode {
    /// Returns the stable wire label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::AllowReadonly => "allow_readonly",
            Self::Ask => "ask",
        }
    }
}

impl fmt::Display for HttpRunApprovalMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Public run lifecycle state owned by the HTTP adapter registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpRunStatus {
    /// The registry has accepted the run but the driver has not acknowledged it yet.
    Starting,
    /// The driver accepted the run.
    Running,
    /// The run is waiting for at least one approval decision.
    WaitingForApproval,
    /// Cancellation has been requested and routed to the driver.
    CancelRequested,
    /// The driver boundary unwound and execution state requires durable reconciliation.
    ExecutionUncertain,
    /// The run has finished.
    Finished,
    /// The run failed or the driver rejected startup.
    Failed,
    /// Cooperative cancellation reached a durable clean terminal.
    Cancelled,
    /// Execution stopped without proving a clean cancellation terminal.
    Interrupted,
}

impl HttpRunStatus {
    /// Returns whether the status is terminal for routing purposes.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Finished | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

/// Typed terminal outcome reported by the production run driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpRunTerminalOutcome {
    /// The shared application run completed successfully.
    Finished,
    /// The shared application run failed.
    Failed,
    /// Cooperative cancellation reached durable quiescence.
    Cancelled,
    /// Execution stopped without a provable clean cancellation terminal.
    Interrupted,
}

impl HttpRunTerminalOutcome {
    /// Returns the terminal lifecycle status projected into run snapshots.
    #[must_use]
    pub const fn status(self) -> HttpRunStatus {
        match self {
            Self::Finished => HttpRunStatus::Finished,
            Self::Failed => HttpRunStatus::Failed,
            Self::Cancelled => HttpRunStatus::Cancelled,
            Self::Interrupted => HttpRunStatus::Interrupted,
        }
    }
}

/// Public snapshot returned by run start/get/cancel endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpRunSnapshot {
    /// HTTP adapter run id.
    pub id: String,
    /// Owning HTTP adapter session id.
    pub session_id: String,
    /// Current adapter-visible run status.
    pub status: HttpRunStatus,
    /// Explicit approval mode provided when the run started.
    pub approval_mode: HttpRunApprovalMode,
    /// Bounded prompt preview for adapter clients.
    pub prompt_preview: String,
    /// Pending approval call ids in deterministic order.
    #[serde(default)]
    pub pending_approval_call_ids: Vec<String>,
    /// Registry-owned state sequence for stale-client command guards.
    pub stream_sequence: u64,
}

/// Pending approval metadata registered by a running HTTP adapter driver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpPendingApproval {
    /// Tool call id awaiting a user decision.
    pub call_id: String,
    /// Tool name shown to clients.
    pub tool_name: String,
    /// Stable id for this approval request.
    pub approval_request_id: String,
    /// Hash of the exact tool call payload being approved.
    pub tool_call_hash: String,
    /// Policy version used to request approval.
    pub policy_version: String,
    /// Expiry timestamp in Unix milliseconds.
    pub expires_at_ms: u64,
}

/// HTTP approval decision payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpApprovalDecisionRequest {
    /// Approval request id echoed from the pending approval snapshot.
    pub approval_request_id: String,
    /// Tool call hash echoed from the pending approval snapshot.
    pub tool_call_hash: String,
    /// Policy version echoed from the pending approval snapshot.
    pub policy_version: String,
    /// Expiry timestamp echoed from the pending approval snapshot.
    pub expires_at_ms: u64,
    /// Explicit decision for the pending approval.
    pub decision: HttpApprovalDecision,
    /// Optional user-facing reason for audit and display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// User decision submitted for one pending approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpApprovalDecision {
    /// Allow the pending tool call.
    Approve,
    /// Deny the pending tool call.
    Deny,
}

impl HttpApprovalDecision {
    /// Maps the HTTP-facing decision to the kernel's persisted approval decision.
    #[must_use]
    pub fn to_user_decision(self) -> ToolApprovalUserDecision {
        match self {
            Self::Approve => ToolApprovalUserDecision::Approved,
            Self::Deny => ToolApprovalUserDecision::Denied,
        }
    }
}

/// Stored and routed approval decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpApprovalDecisionRecord {
    /// Owning run id.
    pub run_id: String,
    /// Tool call id that was resolved.
    pub call_id: String,
    /// Kernel-compatible user decision.
    pub decision: ToolApprovalUserDecision,
    /// Optional user-facing reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Receipt for an envelope-routed approval command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpApprovalCommandReceipt {
    /// Command id used for retry de-duplication.
    pub command_id: String,
    /// Client that submitted the command.
    pub client_id: String,
    /// Session id from the command envelope.
    pub session_id: String,
    /// Run id receiving the approval.
    pub run_id: String,
    /// Tool call id receiving the approval.
    pub call_id: String,
    /// Optional optimistic state guard supplied by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_stream_sequence: Option<u64>,
    /// Optional durable correlation id supplied by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Decision routed to the run driver.
    pub decision: HttpApprovalDecisionRecord,
    /// Whether this response was replayed from a prior command id.
    pub replayed: bool,
}

impl HttpApprovalCommandReceipt {
    pub(crate) fn replayed(mut self) -> Self {
        self.replayed = true;
        self
    }
}

/// Receipt for an envelope-routed run start command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpRunStartCommandReceipt {
    /// Command id used for retry de-duplication.
    pub command_id: String,
    /// Client that submitted the command.
    pub client_id: String,
    /// Session id from the command envelope.
    pub session_id: String,
    /// Optional durable correlation id supplied by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Run snapshot produced by the existing registry/driver path.
    pub run: HttpRunSnapshot,
    /// Whether this response was replayed from a prior command id.
    pub replayed: bool,
}

impl HttpRunStartCommandReceipt {
    pub(crate) fn replayed(mut self) -> Self {
        self.replayed = true;
        self
    }
}

/// Receipt for an envelope-routed run cancel command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpRunCancelCommandReceipt {
    /// Command id used for retry de-duplication.
    pub command_id: String,
    /// Client that submitted the command.
    pub client_id: String,
    /// Session id from the command envelope.
    pub session_id: String,
    /// Optional optimistic state guard supplied by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_stream_sequence: Option<u64>,
    /// Optional durable correlation id supplied by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Run snapshot produced by the existing registry/driver path.
    pub run: HttpRunSnapshot,
    /// Whether this response was replayed from a prior command id.
    pub replayed: bool,
}

impl HttpRunCancelCommandReceipt {
    pub(crate) fn replayed(mut self) -> Self {
        self.replayed = true;
        self
    }
}
