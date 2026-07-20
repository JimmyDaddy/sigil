use std::{fmt, net::SocketAddr};

use serde::{Deserialize, Serialize};

/// Policy identity bound to every V1 HTTP approval request.
pub const HTTP_APPROVAL_POLICY_VERSION: &str = "sigil-http-approval-v1";
use sigil_kernel::{
    TaskVerificationRerunRequest, ToolApprovalUserDecision, VerificationProductView,
};

/// Schema version for the desktop launcher/server metadata handshake.
pub const HTTP_SERVER_INFO_SCHEMA_VERSION: u16 = 4;

/// Authentication mode enforced by the local desktop/app-server adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpServerAuthentication {
    /// Per-launch bearer token supplied outside argv and response payloads.
    Bearer,
}

/// Frozen feature flags a desktop client can use without inspecting OpenAPI text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpServerCapabilities {
    /// Historical workspace catalog is queryable through the authenticated API.
    pub session_catalog: bool,
    /// A catalog candidate can be revalidated and opened as a live adapter session.
    pub durable_session_reopen: bool,
    /// A bound durable session exposes a scope-checked, bounded transcript page.
    pub bounded_transcript_replay: bool,
    /// Durable run events support cursor-bound replay.
    pub durable_event_replay: bool,
    /// Transient and durable run events can be followed while the server is active.
    pub live_events: bool,
    /// Pending tool approvals can be resolved by an authenticated client.
    pub approval: bool,
    /// Active runs support cooperative cancellation and bounded drain.
    pub cancellation: bool,
    /// Durable task verification can be inspected and one exact recommended check rerun.
    pub verification: bool,
    /// Bound sessions expose typed model, approval-mode, and context-usage facts.
    pub run_context: bool,
}

impl HttpServerCapabilities {
    /// Returns the frozen capability set implemented by the desktop V1 bridge.
    #[must_use]
    pub fn desktop_v1() -> Self {
        Self {
            session_catalog: true,
            durable_session_reopen: true,
            bounded_transcript_replay: true,
            durable_event_replay: true,
            live_events: true,
            approval: true,
            cancellation: true,
            verification: true,
            run_context: true,
        }
    }
}

/// Immutable, secret-free metadata published after the local listener is ready.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpServerInfo {
    /// Version of this metadata DTO.
    pub schema_version: u16,
    /// Stable command/event protocol version accepted by the listener.
    pub protocol_version: u16,
    /// Sigil package version that owns the listener.
    pub server_version: String,
    /// Stable identifier for the one workspace owned by this process.
    pub workspace_id: String,
    /// Actual loopback socket address selected after bind.
    pub bind_addr: String,
    /// Authentication scheme enforced by every non-health route.
    pub authentication: HttpServerAuthentication,
    /// Whether owner-pipe EOF is configured as a graceful shutdown trigger.
    pub shutdown_on_stdin_close: bool,
    /// Coarse stable features available to a desktop client.
    pub capabilities: HttpServerCapabilities,
}

impl HttpServerInfo {
    /// Builds metadata for one successfully bound production listener.
    #[must_use]
    pub fn new(
        workspace_id: impl Into<String>,
        bind_addr: SocketAddr,
        shutdown_on_stdin_close: bool,
    ) -> Self {
        Self {
            schema_version: HTTP_SERVER_INFO_SCHEMA_VERSION,
            protocol_version: crate::protocol::HTTP_PROTOCOL_VERSION,
            server_version: env!("CARGO_PKG_VERSION").to_owned(),
            workspace_id: workspace_id.into(),
            bind_addr: bind_addr.to_string(),
            authentication: HttpServerAuthentication::Bearer,
            shutdown_on_stdin_close,
            capabilities: HttpServerCapabilities::desktop_v1(),
        }
    }
}

/// Request body for creating one HTTP adapter session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct HttpSessionCreateRequest {
    /// Optional user-facing label for clients that manage multiple sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Request body for reopening one durable workspace session as a live adapter handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSessionOpenRequest {
    /// Relative direct-child reference returned by the historical session catalog.
    pub session_ref: String,
    /// Durable identity observed with `session_ref`; used as a stale-source guard.
    pub session_id: String,
    /// Optional process-local label. The first successful open wins for duplicate requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Exact durable catalog identity and new bounded display name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSessionRenameRequest {
    pub session_ref: String,
    pub session_id: String,
    pub display_name: String,
}

/// Exact durable catalog identity selected for confirmed deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSessionDeleteRequest {
    pub session_ref: String,
    pub session_id: String,
}

/// Exact invalid catalog source fingerprint selected for quarantine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSessionQuarantineRequest {
    pub session_ref: String,
    pub source_bytes: u64,
    pub source_modified_at_unix_ms: u64,
}

/// Bounded receipt for a committed durable catalog mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpSessionMutationReceipt {
    pub session_ref: String,
    pub session_id: String,
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_generation: Option<u64>,
}

impl From<sigil_runtime::SessionCatalogMutationReceipt> for HttpSessionMutationReceipt {
    fn from(receipt: sigil_runtime::SessionCatalogMutationReceipt) -> Self {
        Self {
            session_ref: receipt.session_ref,
            session_id: receipt.session_id,
            operation_id: receipt.operation_id,
            projection_generation: receipt.projection_generation,
        }
    }
}

/// Bounded receipt for an invalid source moved out of the active catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpSessionQuarantineReceipt {
    pub session_ref: String,
    pub operation_id: String,
    pub quarantine_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_generation: Option<u64>,
}

impl From<sigil_runtime::SessionCatalogQuarantineReceipt> for HttpSessionQuarantineReceipt {
    fn from(receipt: sigil_runtime::SessionCatalogQuarantineReceipt) -> Self {
        Self {
            session_ref: receipt.session_ref,
            operation_id: receipt.operation_id,
            quarantine_name: receipt.quarantine_name,
            projection_generation: receipt.projection_generation,
        }
    }
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

/// User-visible role returned by the bounded transcript endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpTranscriptRole {
    /// User-authored conversation input.
    User,
    /// Assistant-authored output.
    Assistant,
    /// Result of one tool invocation.
    Tool,
}

/// Assistant phase retained for correct transcript presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpTranscriptAssistantKind {
    /// Short assistant lead-in before a tool call.
    ToolPreamble,
    /// Durable progress update.
    Progress,
    /// Durable reasoning trace explicitly classified for UI presentation.
    ReasoningTrace,
    /// Final user-visible answer.
    FinalAnswer,
}

/// One safe message in a bounded transcript page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpSessionTranscriptMessage {
    /// Stable one-based append-only display ordinal.
    pub ordinal: u64,
    /// Stable hashed identity used by clients for reconciliation only.
    pub message_id: String,
    /// Provider-neutral display role.
    pub role: HttpTranscriptRole,
    /// Sanitized, bounded text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Assistant phase when `role=assistant`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_kind: Option<HttpTranscriptAssistantKind>,
    /// Safe tool name resolved without exposing arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Number of omitted safe attachment descriptors.
    pub image_attachment_count: u64,
    /// Whether content was shortened to the per-message bound.
    pub truncated: bool,
    /// Sanitized text size before truncation.
    pub original_content_bytes: u64,
}

/// One chronological page from the server-owned durable transcript projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpSessionTranscriptPage {
    /// Durable session scope revalidated during this read.
    pub session_scope_id: String,
    /// Total user-visible messages observed during this read.
    pub total_messages: u64,
    /// Chronologically ordered page messages.
    pub messages: Vec<HttpSessionTranscriptMessage>,
    /// Exclusive ordinal for the next older page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_before: Option<u64>,
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

/// Model-selection policy for one durable session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpModelSelectionPolicy {
    /// The durable session identity fixes the provider and model for all subsequent runs.
    FixedForSession,
}

/// Evidence source used to resolve a session context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpContextWindowSource {
    /// Provider-owned model metadata supplied the limit.
    Provider,
    /// User configuration supplied the limit.
    Config,
    /// No trustworthy limit is available.
    Unavailable,
}

/// Typed facts used to configure and explain the next run in one bound session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpRunContextView {
    /// Provider identity durably frozen for this session.
    pub provider_name: String,
    /// Model identity durably frozen for this session.
    pub model_name: String,
    /// Whether the model can change without forking the durable session.
    pub model_selection: HttpModelSelectionPolicy,
    /// Default approval mode selected by clients for a new run.
    pub default_approval_mode: HttpRunApprovalMode,
    /// Complete bounded set of approval modes accepted by run start.
    pub available_approval_modes: Vec<HttpRunApprovalMode>,
    /// Effective context limit when one is provable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u32>,
    /// Prompt tokens from the latest durable provider usage snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_prompt_tokens: Option<u64>,
    /// Source used to resolve `context_window_tokens`.
    pub context_window_source: HttpContextWindowSource,
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

/// Exact stale-safe verification rerun request shared with TUI projection truth.
pub type HttpVerificationRerunRequest = TaskVerificationRerunRequest;

/// Renderer-safe verification recommendation and evidence projection.
pub type HttpVerificationView = VerificationProductView;

/// Receipt for an envelope-routed verification rerun command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpVerificationRerunCommandReceipt {
    /// Command id used for retry de-duplication.
    pub command_id: String,
    /// Client that submitted the command.
    pub client_id: String,
    /// Session id from the command envelope.
    pub session_id: String,
    /// Optional durable correlation id supplied by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Refreshed projection after the exact check reached a durable terminal.
    pub verification: HttpVerificationView,
    /// Whether this response was replayed from a prior command id.
    pub replayed: bool,
}

impl HttpVerificationRerunCommandReceipt {
    pub(crate) fn replayed(mut self) -> Self {
        self.replayed = true;
        self
    }
}
