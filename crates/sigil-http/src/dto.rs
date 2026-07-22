use std::{fmt, net::SocketAddr};

use serde::{Deserialize, Serialize};

/// Policy identity bound to every V1 HTTP approval request.
pub const HTTP_APPROVAL_POLICY_VERSION: &str = "sigil-http-approval-v1";
use sigil_kernel::{
    TaskVerificationRerunRequest, ToolApprovalUserDecision, VerificationProductView,
};
use sigil_runtime::conversation_display::{
    ConversationDisplayApprovalDecisionV1, ConversationDisplayAssistantPhaseV1,
    ConversationDisplayCheckpointConflictReasonV1, ConversationDisplayCheckpointOutcomeV1,
    ConversationDisplayContentV1, ConversationDisplayItemKindV1, ConversationDisplayItemV1,
    ConversationDisplayMessageRoleV1, ConversationDisplayPageV1, ConversationDisplaySourceV1,
    ConversationDisplayStatusV1, ConversationTerminalFrontierV1,
};
use sigil_runtime::support::{
    DoctorSupportReportV1, SupportDoctorCheckV1, SupportDoctorStatus, SupportEnvironmentV1,
    SupportPrivacyV1, SupportTerminalFamily,
};

/// Schema version for the desktop launcher/server metadata handshake.
pub const HTTP_SERVER_INFO_SCHEMA_VERSION: u16 = 6;

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
    /// A bound durable session exposes canonical identity/order display pages.
    pub canonical_conversation_display: bool,
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
    /// Bound sessions expose typed model, permission-mode, and context-usage facts.
    pub run_context: bool,
    /// Bound sessions expose a safe, bounded child-agent lifecycle and handoff projection.
    pub agent_activity: bool,
    /// Redacted local diagnostics and an explicit private support-bundle export are available.
    pub support_diagnostics: bool,
}

impl HttpServerCapabilities {
    /// Returns the frozen capability set implemented by the desktop V1 bridge.
    #[must_use]
    pub fn desktop_v1() -> Self {
        Self {
            session_catalog: true,
            durable_session_reopen: true,
            bounded_transcript_replay: true,
            canonical_conversation_display: true,
            durable_event_replay: true,
            live_events: true,
            approval: true,
            cancellation: true,
            verification: true,
            run_context: true,
            agent_activity: true,
            support_diagnostics: true,
        }
    }
}

/// Stable status token for the desktop support surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpSupportStatus {
    Ok,
    Warn,
    Error,
}

impl From<SupportDoctorStatus> for HttpSupportStatus {
    fn from(value: SupportDoctorStatus) -> Self {
        match value {
            SupportDoctorStatus::Ok => Self::Ok,
            SupportDoctorStatus::Warn => Self::Warn,
            SupportDoctorStatus::Error => Self::Error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSupportSummary {
    pub overall_status: HttpSupportStatus,
    pub ok: usize,
    pub warn: usize,
    pub error: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSupportCheck {
    pub status: HttpSupportStatus,
    pub name: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl From<SupportDoctorCheckV1> for HttpSupportCheck {
    fn from(value: SupportDoctorCheckV1) -> Self {
        Self {
            status: value.status.into(),
            name: value.name,
            summary: value.summary,
            remediation: value.remediation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSupportEnvironment {
    pub os: String,
    pub architecture: String,
    pub terminal_family: String,
}

impl From<SupportEnvironmentV1> for HttpSupportEnvironment {
    fn from(value: SupportEnvironmentV1) -> Self {
        let terminal_family = match value.terminal_family {
            SupportTerminalFamily::Iterm2 => "iterm2",
            SupportTerminalFamily::AppleTerminal => "apple_terminal",
            SupportTerminalFamily::Wezterm => "wezterm",
            SupportTerminalFamily::Vscode => "vscode",
            SupportTerminalFamily::Other => "other",
            SupportTerminalFamily::Unknown => "unknown",
        };
        Self {
            os: value.os,
            architecture: value.architecture,
            terminal_family: terminal_family.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSupportPrivacy {
    pub included: Vec<String>,
    pub excluded: Vec<String>,
    pub review_before_sharing: bool,
}

impl From<SupportPrivacyV1> for HttpSupportPrivacy {
    fn from(value: SupportPrivacyV1) -> Self {
        Self {
            included: value.included,
            excluded: value.excluded,
            review_before_sharing: value.review_before_sharing,
        }
    }
}

/// Path-free diagnostic projection returned to an authenticated desktop client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSupportDoctorReport {
    pub generated_at_unix_ms: u64,
    pub version: String,
    pub commit: String,
    pub target: String,
    pub profile: String,
    pub environment: HttpSupportEnvironment,
    pub summary: HttpSupportSummary,
    pub checks: Vec<HttpSupportCheck>,
    pub privacy: HttpSupportPrivacy,
}

impl From<DoctorSupportReportV1> for HttpSupportDoctorReport {
    fn from(value: DoctorSupportReportV1) -> Self {
        Self {
            generated_at_unix_ms: value.generated_at_unix_ms,
            version: value.build.version,
            commit: value.build.commit,
            target: value.build.target,
            profile: value.build.profile,
            environment: value.environment.into(),
            summary: HttpSupportSummary {
                overall_status: value.summary.overall_status.into(),
                ok: value.summary.ok,
                warn: value.summary.warn,
                error: value.summary.error,
            },
            checks: value.checks.into_iter().map(Into::into).collect(),
            privacy: value.privacy.into(),
        }
    }
}

/// Bounded private support JSON handed only to the native desktop save boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSupportBundleExport {
    pub suggested_file_name: String,
    pub generated_at_unix_ms: u64,
    pub content: String,
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
    /// Optional model for the new durable session, selected from the run-context offer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
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

/// Exact invalid catalog source fingerprint selected for permanent deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSessionInvalidSourceDeleteRequest {
    pub session_ref: String,
    pub source_bytes: u64,
    pub source_modified_at_unix_ms: u64,
}

/// Server-owned operation admitted by the session catalog batch planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpSessionCatalogBatchAction {
    DeleteSessions,
    QuarantineInvalidSources,
    DeleteInvalidSources,
}

/// One exact catalog identity selected by an interactive client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSessionCatalogBatchItem {
    pub session_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_modified_at_unix_ms: Option<u64>,
}

/// Exact selected set submitted for a read-only batch preflight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSessionCatalogBatchPlanRequest {
    pub action: HttpSessionCatalogBatchAction,
    pub items: Vec<HttpSessionCatalogBatchItem>,
}

/// The same selected set plus the opaque plan digest confirmed by the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSessionCatalogBatchExecuteRequest {
    pub plan_id: String,
    pub action: HttpSessionCatalogBatchAction,
    pub items: Vec<HttpSessionCatalogBatchItem>,
}

/// Server classification for one preflight row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpSessionCatalogBatchPlanStatus {
    Executable,
    Blocked,
}

/// One bounded preflight result. `reason` is a stable machine code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpSessionCatalogBatchPlanItem {
    pub session_ref: String,
    pub status: HttpSessionCatalogBatchPlanStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Content-bound preview returned before any batch mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpSessionCatalogBatchPlan {
    pub plan_id: String,
    pub action: HttpSessionCatalogBatchAction,
    pub generation: u64,
    pub total: usize,
    pub executable: usize,
    pub blocked: usize,
    pub items: Vec<HttpSessionCatalogBatchPlanItem>,
}

/// Outcome of one item in a best-effort batch execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpSessionCatalogBatchOutcome {
    Completed,
    Failed,
    Skipped,
}

/// Bounded per-item batch receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpSessionCatalogBatchReceiptItem {
    pub session_ref: String,
    pub outcome: HttpSessionCatalogBatchOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_generation: Option<u64>,
}

/// Result of one server-owned best-effort batch execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpSessionCatalogBatchReceipt {
    pub plan_id: String,
    pub action: HttpSessionCatalogBatchAction,
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub items: Vec<HttpSessionCatalogBatchReceiptItem>,
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

/// Bounded receipt for one invalid source permanently removed from the active catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpSessionInvalidSourceDeleteReceipt {
    pub session_ref: String,
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_generation: Option<u64>,
}

impl From<sigil_runtime::SessionCatalogInvalidSourceDeleteReceipt>
    for HttpSessionInvalidSourceDeleteReceipt
{
    fn from(receipt: sigil_runtime::SessionCatalogInvalidSourceDeleteReceipt) -> Self {
        Self {
            session_ref: receipt.session_ref,
            operation_id: receipt.operation_id,
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

/// Read-only durable frontier revalidated for one bound session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpDurableSessionFrontier {
    /// Highest durable session-stream sequence visible to this probe.
    pub through_stream_sequence: u64,
}

/// Exact process-local foreground owner returned by one fresh continuity probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpForegroundRunOwner {
    /// Active run that owns this adapter session.
    pub run_id: String,
    /// Opaque owner generation echoed by exact attach admission.
    pub owner_revision: String,
}

/// Recovery actions a client may offer without inferring capability from error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpContinuityRecoveryAction {
    RetryCurrent,
    OpenAnotherWorkspace,
    OpenDiagnostics,
    ShowDetails,
    ContinueReadOnly,
}

/// Fresh continuity proof for one process-local adapter session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSessionContinuityView {
    /// Durable scope revalidated by the runtime. Native IPC must not project this field.
    pub durable_session_scope_id: String,
    /// Current read-only durable frontier.
    pub durable_frontier: HttpDurableSessionFrontier,
    /// Current process-local foreground owner, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_owner: Option<HttpForegroundRunOwner>,
    /// Bounded recovery actions allowed for the current owner state.
    #[serde(default)]
    pub recovery_actions: Vec<HttpContinuityRecoveryAction>,
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

/// Durable order projected for one canonical conversation item.
///
/// The stream sequence is encoded as decimal text so JavaScript clients cannot lose precision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationDisplayOrder {
    pub session_stream_sequence: String,
    pub subindex: u32,
}

/// Provider-neutral visual category for one canonical item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpConversationDisplayItemKind {
    UserMessage,
    Reasoning,
    AssistantMessage,
    Tool,
    Approval,
    Checkpoint,
    Notice,
    Terminal,
}

/// Durable evidence class behind one canonical item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpConversationDisplaySource {
    DurableTranscript,
    DurableRunEvent,
    LiveTransient,
}

/// Bounded lifecycle vocabulary used by canonical items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpConversationDisplayStatus {
    Recorded,
    Requested,
    WaitingForApproval,
    Approved,
    Denied,
    Completed,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    Blocked,
}

/// Provider-neutral message author.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpConversationDisplayMessageRole {
    User,
    Assistant,
}

/// Assistant phase retained for canonical renderer presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpConversationDisplayAssistantPhase {
    ToolPreamble,
    Progress,
    FinalAnswer,
}

/// User decision recorded for one approval item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpConversationDisplayApprovalDecision {
    Approved,
    ApprovedForSession,
    Denied,
}

/// Durable checkpoint outcome shown by the canonical renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpConversationDisplayCheckpointOutcome {
    Restored,
    Conflict,
}

/// Bounded checkpoint conflict vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpConversationDisplayCheckpointConflictReason {
    WorkspaceMismatch,
    CurrentHashMismatch,
    ArtifactUnavailable,
    SensitiveSnapshot,
    UnsupportedSnapshot,
    InvalidBinding,
}

/// Typed, secret-safe content carried by one canonical display item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", deny_unknown_fields)]
pub enum HttpConversationDisplayContent {
    Message {
        role: HttpConversationDisplayMessageRole,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assistant_phase: Option<HttpConversationDisplayAssistantPhase>,
        image_attachment_count: u64,
        truncated: bool,
        original_content_bytes: u64,
    },
    Reasoning {
        text: String,
        truncated: bool,
        original_content_bytes: u64,
    },
    Tool {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        truncated: bool,
        original_content_bytes: u64,
    },
    Approval {
        call_id: String,
        tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<HttpConversationDisplayApprovalDecision>,
    },
    Checkpoint {
        outcome: HttpConversationDisplayCheckpointOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conflict_reason: Option<HttpConversationDisplayCheckpointConflictReason>,
    },
    Notice {
        text: String,
        truncated: bool,
        original_content_bytes: u64,
    },
    Terminal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_message_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        safe_summary: Option<String>,
        summary_truncated: bool,
    },
}

/// One canonical, durable display item safe for authenticated local clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationDisplayItem {
    pub schema_version: u16,
    pub display_id: String,
    pub display_order: HttpConversationDisplayOrder,
    pub source_event_id: String,
    pub kind: HttpConversationDisplayItemKind,
    pub source: HttpConversationDisplaySource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_sequence: Option<String>,
    pub status: HttpConversationDisplayStatus,
    pub content: HttpConversationDisplayContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciles: Option<Vec<String>>,
}

/// Latest proven terminal boundary at the page's fixed durable frontier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationTerminalFrontier {
    pub run_id: String,
    pub session_stream_sequence: String,
    pub status: HttpConversationDisplayStatus,
}

/// Gap fact retained for clients without exposing journal or filesystem details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationDisplayGapFact {
    pub kind: HttpConversationDisplayGapKind,
    pub after_session_stream_sequence: String,
}

/// Bounded gap vocabulary for future retention/replay projections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpConversationDisplayGapKind {
    Retention,
    Replay,
}

/// Process-local run anchor observed after the durable page was projected.
///
/// This anchor is explicitly provisional and never supplies durable display order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationLiveProvisionalAnchor {
    pub durable_frontier: String,
    pub run_id: String,
    pub run_sequence: String,
}

/// Opaque-cursor page over canonical durable conversation display items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationDisplayPage {
    pub schema_version: u16,
    /// Process-local adapter session id; the raw durable scope is intentionally omitted.
    pub request_scope: String,
    pub through_session_stream_sequence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_frontier: Option<HttpConversationTerminalFrontier>,
    pub total_items: String,
    pub items: Vec<HttpConversationDisplayItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
    #[serde(default)]
    pub gap_facts: Vec<HttpConversationDisplayGapFact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_provisional_anchor: Option<HttpConversationLiveProvisionalAnchor>,
}

impl HttpConversationDisplayPage {
    pub(crate) fn from_runtime(request_scope: &str, page: ConversationDisplayPageV1) -> Self {
        Self {
            schema_version: page.schema_version,
            request_scope: request_scope.to_owned(),
            through_session_stream_sequence: page.through_session_stream_sequence.to_string(),
            terminal_frontier: page.terminal_frontier.map(Into::into),
            total_items: page.total_items.to_string(),
            items: page.items.into_iter().map(Into::into).collect(),
            next_cursor: page.next_cursor,
            has_more: page.has_more,
            gap_facts: Vec::new(),
            live_provisional_anchor: None,
        }
    }
}

impl From<ConversationTerminalFrontierV1> for HttpConversationTerminalFrontier {
    fn from(frontier: ConversationTerminalFrontierV1) -> Self {
        Self {
            run_id: frontier.run_id,
            session_stream_sequence: frontier.session_stream_sequence.to_string(),
            status: frontier.status.into(),
        }
    }
}

impl From<ConversationDisplayItemV1> for HttpConversationDisplayItem {
    fn from(item: ConversationDisplayItemV1) -> Self {
        Self {
            schema_version: item.schema_version,
            display_id: item.display_id,
            display_order: HttpConversationDisplayOrder {
                session_stream_sequence: item.display_order.session_stream_sequence.to_string(),
                subindex: item.display_order.subindex,
            },
            source_event_id: item.source_event_id,
            kind: item.kind.into(),
            source: item.source.into(),
            run_id: item.run_id,
            run_sequence: item.run_sequence.map(|sequence| sequence.to_string()),
            status: item.status.into(),
            content: item.content.into(),
            reconciles: item.reconciles,
        }
    }
}

impl From<ConversationDisplayContentV1> for HttpConversationDisplayContent {
    fn from(content: ConversationDisplayContentV1) -> Self {
        match content {
            ConversationDisplayContentV1::Message {
                role,
                text,
                assistant_phase,
                image_attachment_count,
                truncated,
                original_content_bytes,
            } => Self::Message {
                role: role.into(),
                text,
                assistant_phase: assistant_phase.map(Into::into),
                image_attachment_count: usize_as_u64(image_attachment_count),
                truncated,
                original_content_bytes: usize_as_u64(original_content_bytes),
            },
            ConversationDisplayContentV1::Reasoning {
                text,
                truncated,
                original_content_bytes,
            } => Self::Reasoning {
                text,
                truncated,
                original_content_bytes: usize_as_u64(original_content_bytes),
            },
            ConversationDisplayContentV1::Tool {
                call_id,
                tool_name,
                output,
                truncated,
                original_content_bytes,
            } => Self::Tool {
                call_id,
                tool_name,
                output,
                truncated,
                original_content_bytes: usize_as_u64(original_content_bytes),
            },
            ConversationDisplayContentV1::Approval {
                call_id,
                tool_name,
                decision,
            } => Self::Approval {
                call_id,
                tool_name,
                decision: decision.map(Into::into),
            },
            ConversationDisplayContentV1::Checkpoint {
                outcome,
                checkpoint_id,
                conflict_reason,
            } => Self::Checkpoint {
                outcome: outcome.into(),
                checkpoint_id,
                conflict_reason: conflict_reason.map(Into::into),
            },
            ConversationDisplayContentV1::Notice {
                text,
                truncated,
                original_content_bytes,
            } => Self::Notice {
                text,
                truncated,
                original_content_bytes: usize_as_u64(original_content_bytes),
            },
            ConversationDisplayContentV1::Terminal {
                final_message_id,
                safe_summary,
                summary_truncated,
            } => Self::Terminal {
                final_message_id,
                safe_summary,
                summary_truncated,
            },
        }
    }
}

macro_rules! map_enum {
    ($source:ty => $target:ty { $($variant:ident),+ $(,)? }) => {
        impl From<$source> for $target {
            fn from(value: $source) -> Self {
                match value {
                    $(<$source>::$variant => <$target>::$variant,)+
                }
            }
        }
    };
}

map_enum!(ConversationDisplayItemKindV1 => HttpConversationDisplayItemKind {
    UserMessage, Reasoning, AssistantMessage, Tool, Approval, Checkpoint, Notice, Terminal
});
map_enum!(ConversationDisplaySourceV1 => HttpConversationDisplaySource {
    DurableTranscript, DurableRunEvent, LiveTransient
});
map_enum!(ConversationDisplayStatusV1 => HttpConversationDisplayStatus {
    Recorded, Requested, WaitingForApproval, Approved, Denied, Completed, Succeeded, Failed,
    Cancelled, Interrupted, Blocked
});
map_enum!(ConversationDisplayMessageRoleV1 => HttpConversationDisplayMessageRole {
    User, Assistant
});
map_enum!(ConversationDisplayAssistantPhaseV1 => HttpConversationDisplayAssistantPhase {
    ToolPreamble, Progress, FinalAnswer
});
map_enum!(ConversationDisplayApprovalDecisionV1 => HttpConversationDisplayApprovalDecision {
    Approved, ApprovedForSession, Denied
});
map_enum!(ConversationDisplayCheckpointOutcomeV1 => HttpConversationDisplayCheckpointOutcome {
    Restored, Conflict
});
map_enum!(ConversationDisplayCheckpointConflictReasonV1 => HttpConversationDisplayCheckpointConflictReason {
    WorkspaceMismatch, CurrentHashMismatch, ArtifactUnavailable, SensitiveSnapshot,
    UnsupportedSnapshot, InvalidBinding
});

fn usize_as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Provider-neutral child-agent lifecycle visible to authenticated application clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpAgentActivityStatus {
    Started,
    Running,
    Blocked,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Unavailable,
    Unknown,
}

/// Whether a terminal child result is still pending or has reached the parent conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpAgentHandoffStatus {
    Pending,
    ResultReady,
    ResultRead,
    Returned,
    Unavailable,
}

/// Bounded token usage for one child-agent result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpAgentUsageSummary {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
}

/// One safe activity row. Session references, paths, hashes, and raw tool arguments are omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpAgentActivityItem {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub objective: String,
    pub status: HttpAgentActivityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub handoff_status: HttpAgentHandoffStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    pub result_summary_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<HttpAgentUsageSummary>,
}

/// Bounded child-agent activity for one parent session, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpAgentActivityView {
    pub total_agents: usize,
    pub active_agents: usize,
    pub terminal_agents: usize,
    pub items: Vec<HttpAgentActivityItem>,
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
    /// Optional model selected for this run in the existing durable session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    /// Opaque run-context binding required with an explicit model selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_selection_binding: Option<String>,
    /// Explicit user-facing permission mode for the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<HttpPermissionMode>,
    /// Explicit exact-provider/model reasoning effort selected for the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<HttpReasoningEffort>,
    /// Opaque run-context binding required with an explicit reasoning effort.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort_binding: Option<String>,
    /// Exact catalog binding for one user-invoked inline skill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_binding: Option<HttpApplicationSkillBinding>,
    /// Exact catalog binding for one user-invoked supervised agent profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_binding: Option<HttpApplicationAgentBinding>,
}

/// Request body for cancelling one run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct HttpRunCancelRequest {
    /// Optional user-facing reason for diagnostics and future audit surfaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Permission mode accepted by the HTTP run start endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HttpPermissionMode {
    ReadOnly,
    Manual,
    AutoEdit,
    DangerFullAccess,
}

/// Reasoning effort accepted by the shared application run contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpReasoningEffort {
    Low,
    Medium,
    High,
    Max,
}

/// Model-selection policy for one durable session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpModelSelectionPolicy {
    /// Each run may select an admitted model while retaining the durable session and transcript.
    PerRun,
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

/// Client-owned action behind an available application command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpApplicationClientAction {
    NewSession,
    FocusEffort,
    FocusModel,
    OpenSessionPicker,
    OpenAgentWorkbench,
    OpenSettings,
    OpenSupport,
}

/// One bounded slash-command catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpApplicationCommandCatalogEntry {
    pub canonical: String,
    pub aliases: Vec<String>,
    pub label: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    pub completes_with_space: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_action: Option<HttpApplicationClientAction>,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

/// Exact digest binding required to invoke one inline skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpApplicationSkillBinding {
    pub skill_id: String,
    pub skill_sha256: String,
    pub index_fingerprint: String,
}

/// Exact immutable binding for one user-invoked agent profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpApplicationAgentBinding {
    pub profile_id: String,
    pub snapshot_id: String,
}

/// One path-free skill catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpApplicationSkillCatalogEntry {
    pub id: String,
    pub invocation_token: String,
    pub name: String,
    pub description: String,
    pub source: String,
    pub run_mode: String,
    pub trust: String,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<HttpApplicationSkillBinding>,
}

/// One path-free agent profile catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpApplicationAgentCatalogEntry {
    pub id: String,
    pub invocation_token: String,
    pub description: String,
    pub source: String,
    pub kind: String,
    pub trust: String,
    pub enabled: bool,
    pub user_invocable: bool,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<HttpApplicationAgentBinding>,
}

/// Bounded extension metadata used by graphical application clients.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpApplicationExtensionCatalog {
    pub commands: Vec<HttpApplicationCommandCatalogEntry>,
    pub skills: Vec<HttpApplicationSkillCatalogEntry>,
    pub agents: Vec<HttpApplicationAgentCatalogEntry>,
}

/// Exact reasoning-effort capabilities for one model selectable in the current session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpApplicationModelOption {
    pub model_name: String,
    pub available_reasoning_efforts: Vec<HttpReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<HttpReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort_binding: Option<String>,
}

/// Typed facts used to configure and explain the next run in one bound session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpRunContextView {
    /// Provider identity durably frozen for this session.
    pub provider_name: String,
    /// Model identity durably frozen for this session.
    pub model_name: String,
    /// Models accepted when creating a new session for this provider.
    pub available_models: Vec<String>,
    /// Exact effort capabilities for each selectable model.
    pub model_options: Vec<HttpApplicationModelOption>,
    /// Whether the model can change without forking the durable session.
    pub model_selection: HttpModelSelectionPolicy,
    /// Opaque binding proving the exact current and available model set.
    pub model_selection_binding: String,
    /// Configured permission mode selected by clients for a new run.
    pub default_permission_mode: HttpPermissionMode,
    /// Complete bounded set of permission modes accepted by run start.
    pub available_permission_modes: Vec<HttpPermissionMode>,
    /// Exact values supported by this durable provider and model.
    pub available_reasoning_efforts: Vec<HttpReasoningEffort>,
    /// Configured default when it belongs to the exact support set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<HttpReasoningEffort>,
    /// Opaque provider/model capability binding echoed with an explicit run selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort_binding: Option<String>,
    /// Effective context limit when one is provable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u32>,
    /// Prompt tokens from the latest durable provider usage snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_prompt_tokens: Option<u64>,
    /// Source used to resolve `context_window_tokens`.
    pub context_window_source: HttpContextWindowSource,
    /// Bounded command, skill, and agent metadata for this workspace and session.
    pub extension_catalog: HttpApplicationExtensionCatalog,
}

impl HttpPermissionMode {
    /// Returns the stable wire label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Manual => "manual",
            Self::AutoEdit => "auto-edit",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

impl From<HttpPermissionMode> for sigil_kernel::PermissionMode {
    fn from(value: HttpPermissionMode) -> Self {
        match value {
            HttpPermissionMode::ReadOnly => Self::ReadOnly,
            HttpPermissionMode::Manual => Self::Manual,
            HttpPermissionMode::AutoEdit => Self::AutoEdit,
            HttpPermissionMode::DangerFullAccess => Self::DangerFullAccess,
        }
    }
}

impl From<sigil_kernel::PermissionMode> for HttpPermissionMode {
    fn from(value: sigil_kernel::PermissionMode) -> Self {
        match value {
            sigil_kernel::PermissionMode::ReadOnly => Self::ReadOnly,
            sigil_kernel::PermissionMode::Manual => Self::Manual,
            sigil_kernel::PermissionMode::AutoEdit => Self::AutoEdit,
            sigil_kernel::PermissionMode::DangerFullAccess => Self::DangerFullAccess,
        }
    }
}

impl From<HttpReasoningEffort> for sigil_kernel::ReasoningEffort {
    fn from(value: HttpReasoningEffort) -> Self {
        match value {
            HttpReasoningEffort::Low => Self::Low,
            HttpReasoningEffort::Medium => Self::Medium,
            HttpReasoningEffort::High => Self::High,
            HttpReasoningEffort::Max => Self::Max,
        }
    }
}

impl From<sigil_kernel::ReasoningEffort> for HttpReasoningEffort {
    fn from(value: sigil_kernel::ReasoningEffort) -> Self {
        match value {
            sigil_kernel::ReasoningEffort::Low => Self::Low,
            sigil_kernel::ReasoningEffort::Medium => Self::Medium,
            sigil_kernel::ReasoningEffort::High => Self::High,
            sigil_kernel::ReasoningEffort::Max => Self::Max,
        }
    }
}

impl fmt::Display for HttpPermissionMode {
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
    /// Explicit permission mode provided when the run started.
    pub permission_mode: HttpPermissionMode,
    /// Explicit reasoning effort bound to this run, when the provider supports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<HttpReasoningEffort>,
    /// Bounded prompt preview for adapter clients.
    pub prompt_preview: String,
    /// Pending approval call ids in deterministic order.
    #[serde(default)]
    pub pending_approval_call_ids: Vec<String>,
    /// Registry-owned state sequence for stale-client command guards.
    pub stream_sequence: u64,
}

/// Schema version for the bounded conversation queue application view.
pub const HTTP_CONVERSATION_QUEUE_SCHEMA_VERSION: u16 = 1;

/// Maximum queue rows returned to one local application client.
pub const HTTP_MAX_CONVERSATION_QUEUE_ITEMS: usize = 100;

/// Opaque compare-and-swap generation for one exact durable queue projection.
///
/// Clients must echo this value unchanged and must not infer ordering from its contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HttpConversationQueueGeneration(pub String);

/// Product-level class of one queued input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpConversationQueueItemKind {
    Chat,
    PlanPrompt,
    AgentMention,
    AgentMessage,
    Unknown,
}

/// Durable lifecycle projected for one queued input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpConversationQueueItemStatus {
    Queued,
    Dispatching,
    Delivered,
    Rejected,
    Cancelled,
    Stale,
    Unknown,
}

/// Availability of the exact prompt material required for future dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpConversationQueuePromptMaterial {
    /// The durable prompt is already an exact safe value.
    PersistedSafe,
    /// Exact material is bound to the current application owner only.
    AvailableProcessLocal,
    /// Exact material was intentionally lost and the user must enter it again.
    RequiresReentry,
}

/// Typed reason why a queue item cannot currently be promoted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpConversationQueueBlockedReason {
    QueuePaused,
    RequiresReentry,
    ForegroundRunActive,
    WaitingForTerminalFrontier,
    ForegroundOwnerLost,
    PermissionRequired,
    Conflict,
    Stale,
    Terminal,
    UnsupportedTarget,
    MaterialUnavailable,
}

/// One bounded, secret-free queue row. Exact prompt material and prompt hashes are excluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationQueueItem {
    pub entry_id: String,
    pub order: u32,
    pub kind: HttpConversationQueueItemKind,
    pub status: HttpConversationQueueItemStatus,
    pub prompt_preview: String,
    pub prompt_preview_truncated: bool,
    pub prompt_material: HttpConversationQueuePromptMaterial,
    pub dispatchable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<HttpConversationQueueBlockedReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<u64>,
}

/// Bounded queue projection for one exact application session scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationQueueView {
    pub schema_version: u16,
    pub session_id: String,
    pub generation: HttpConversationQueueGeneration,
    pub paused: bool,
    pub total_items: u32,
    pub items: Vec<HttpConversationQueueItem>,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_dispatchable_entry_id: Option<String>,
}

/// Stable operation label returned without echoing exact prompt material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpConversationQueueCommandActionKind {
    Enqueue,
    Edit,
    Remove,
    Reorder,
    Pause,
    Resume,
    InterruptAndRunNext,
}

/// Exact queue mutation submitted inside the existing idempotent command envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpConversationQueueCommandAction {
    Enqueue {
        prompt: String,
        kind: HttpConversationQueueItemKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<HttpReasoningEffort>,
    },
    Edit {
        entry_id: String,
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<HttpReasoningEffort>,
    },
    Remove {
        entry_id: String,
    },
    Reorder {
        entry_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_entry_id: Option<String>,
    },
    Pause,
    Resume,
    InterruptAndRunNext {
        foreground_run_id: String,
        foreground_owner_revision: String,
    },
}

impl HttpConversationQueueCommandAction {
    /// Returns a content-free operation label suitable for receipts and audit projection.
    #[must_use]
    pub const fn kind(&self) -> HttpConversationQueueCommandActionKind {
        match self {
            Self::Enqueue { .. } => HttpConversationQueueCommandActionKind::Enqueue,
            Self::Edit { .. } => HttpConversationQueueCommandActionKind::Edit,
            Self::Remove { .. } => HttpConversationQueueCommandActionKind::Remove,
            Self::Reorder { .. } => HttpConversationQueueCommandActionKind::Reorder,
            Self::Pause => HttpConversationQueueCommandActionKind::Pause,
            Self::Resume => HttpConversationQueueCommandActionKind::Resume,
            Self::InterruptAndRunNext { .. } => {
                HttpConversationQueueCommandActionKind::InterruptAndRunNext
            }
        }
    }
}

/// Queue-specific compare-and-swap payload carried by `HttpCommandEnvelope`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationQueueCommandRequest {
    pub expected_generation: HttpConversationQueueGeneration,
    pub action: HttpConversationQueueCommandAction,
}

/// Durable queue mutation receipt. Exact prompt material is never echoed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationQueueCommandReceipt {
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    pub action: HttpConversationQueueCommandActionKind,
    pub expected_generation: HttpConversationQueueGeneration,
    pub generation: HttpConversationQueueGeneration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupt_owner: Option<HttpForegroundRunOwner>,
    pub queue: HttpConversationQueueView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub replayed: bool,
}

impl HttpConversationQueueCommandReceipt {
    pub(crate) fn replayed(mut self) -> Self {
        self.replayed = true;
        self
    }
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
    /// Whether this exact approval may create a bounded session-local grant.
    #[serde(default)]
    pub session_grant_available: bool,
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
    /// Allow this call and equivalent bounded calls for the current session.
    ApproveForSession,
    /// Deny the pending tool call.
    Deny,
}

impl HttpApprovalDecision {
    /// Maps the HTTP-facing decision to the kernel's persisted approval decision.
    #[must_use]
    pub fn to_user_decision(self) -> ToolApprovalUserDecision {
        match self {
            Self::Approve => ToolApprovalUserDecision::Approved,
            Self::ApproveForSession => ToolApprovalUserDecision::ApprovedForSession,
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
    /// Exact foreground owner admitted for the initial live follower, when still active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_owner: Option<HttpForegroundRunOwner>,
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
