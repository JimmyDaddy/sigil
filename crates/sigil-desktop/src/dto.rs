use std::fmt;

use serde::{Deserialize, Serialize};

/// Current command-envelope protocol accepted by `sigil serve`.
pub const DESKTOP_HTTP_PROTOCOL_VERSION: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopSupportStatus {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSupportSummary {
    pub overall_status: DesktopSupportStatus,
    pub ok: usize,
    pub warn: usize,
    pub error: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSupportCheck {
    pub status: DesktopSupportStatus,
    pub name: String,
    pub summary: String,
    #[serde(default)]
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSupportEnvironment {
    pub os: String,
    pub architecture: String,
    pub terminal_family: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSupportPrivacy {
    pub included: Vec<String>,
    pub excluded: Vec<String>,
    pub review_before_sharing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSupportDoctorReport {
    pub generated_at_unix_ms: u64,
    pub version: String,
    pub commit: String,
    pub target: String,
    pub profile: String,
    pub environment: DesktopSupportEnvironment,
    pub summary: DesktopSupportSummary,
    pub checks: Vec<DesktopSupportCheck>,
    pub privacy: DesktopSupportPrivacy,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSupportBundleExport {
    pub suggested_file_name: String,
    pub generated_at_unix_ms: u64,
    pub content: String,
}

/// Request body for creating one process-local session handle.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(default, rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionCreateRequest {
    /// Optional user-visible label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Optional model for the new durable session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
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

/// Exact durable catalog identity and new display name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionRenameRequest {
    pub session_ref: String,
    pub session_id: String,
    pub display_name: String,
}

/// Exact durable catalog identity selected for confirmed deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionDeleteRequest {
    pub session_ref: String,
    pub session_id: String,
}

/// Exact invalid source fingerprint selected for native-shell quarantine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionQuarantineRequest {
    pub session_ref: String,
    pub source_bytes: u64,
    pub source_modified_at_unix_ms: u64,
}

/// Exact invalid source fingerprint selected for native-shell permanent deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionInvalidSourceDeleteRequest {
    pub session_ref: String,
    pub source_bytes: u64,
    pub source_modified_at_unix_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopSessionCatalogBatchAction {
    DeleteSessions,
    QuarantineInvalidSources,
    DeleteInvalidSources,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionCatalogBatchItem {
    pub session_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_modified_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionCatalogBatchPlanRequest {
    pub action: DesktopSessionCatalogBatchAction,
    pub items: Vec<DesktopSessionCatalogBatchItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionCatalogBatchExecuteRequest {
    pub plan_id: String,
    pub action: DesktopSessionCatalogBatchAction,
    pub items: Vec<DesktopSessionCatalogBatchItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopSessionCatalogBatchPlanStatus {
    Executable,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionCatalogBatchPlanItem {
    pub session_ref: String,
    pub status: DesktopSessionCatalogBatchPlanStatus,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionCatalogBatchPlan {
    pub plan_id: String,
    pub action: DesktopSessionCatalogBatchAction,
    pub generation: u64,
    pub total: usize,
    pub executable: usize,
    pub blocked: usize,
    pub items: Vec<DesktopSessionCatalogBatchPlanItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopSessionCatalogBatchOutcome {
    Completed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionCatalogBatchReceiptItem {
    pub session_ref: String,
    pub outcome: DesktopSessionCatalogBatchOutcome,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub quarantine_name: Option<String>,
    #[serde(default)]
    pub projection_generation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionCatalogBatchReceipt {
    pub plan_id: String,
    pub action: DesktopSessionCatalogBatchAction,
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub items: Vec<DesktopSessionCatalogBatchReceiptItem>,
}

/// Bounded receipt for a committed durable catalog mutation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionMutationReceipt {
    pub session_ref: String,
    pub session_id: String,
    pub operation_id: String,
    #[serde(default)]
    pub projection_generation: Option<u64>,
}

/// Bounded receipt for one invalid source moved out of the active catalog.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionQuarantineReceipt {
    pub session_ref: String,
    pub operation_id: String,
    pub quarantine_name: String,
    #[serde(default)]
    pub projection_generation: Option<u64>,
}

/// Bounded receipt for one invalid source permanently removed from the active catalog.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionInvalidSourceDeleteReceipt {
    pub session_ref: String,
    pub operation_id: String,
    #[serde(default)]
    pub projection_generation: Option<u64>,
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

/// Read-only durable frontier returned by one continuity probe.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopDurableSessionFrontier {
    pub through_stream_sequence: u64,
}

/// Exact process-local foreground owner and its opaque attach revision.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopForegroundRunOwner {
    pub run_id: String,
    pub owner_revision: String,
}

/// Server-admitted recovery action for a continuity state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopContinuityRecoveryAction {
    RetryCurrent,
    OpenAnotherWorkspace,
    OpenDiagnostics,
    ShowDetails,
    ContinueReadOnly,
}

/// Fresh durable-frontier and foreground-owner proof from the authenticated server.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionContinuityView {
    /// Private durable scope used only by the native attachment boundary.
    pub durable_session_scope_id: String,
    pub durable_frontier: DesktopDurableSessionFrontier,
    #[serde(default)]
    pub foreground_owner: Option<DesktopForegroundRunOwner>,
    #[serde(default)]
    pub recovery_actions: Vec<DesktopContinuityRecoveryAction>,
}

impl fmt::Debug for DesktopSessionContinuityView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopSessionContinuityView")
            .field("durable_session_scope_id", &"<redacted>")
            .field("durable_frontier", &self.durable_frontier)
            .field("foreground_owner", &self.foreground_owner)
            .field("recovery_actions", &self.recovery_actions)
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

/// Permission mode accepted by a run-start command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesktopPermissionMode {
    ReadOnly,
    Manual,
    AutoEdit,
    DangerFullAccess,
}

/// Model-selection policy projected by the server for one durable session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopModelSelectionPolicy {
    PerRun,
}

/// Evidence source used to resolve a session context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopContextWindowSource {
    Provider,
    Config,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopApplicationClientAction {
    NewSession,
    FocusEffort,
    FocusModel,
    OpenSessionPicker,
    OpenAgentWorkbench,
    OpenSettings,
    OpenSupport,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopApplicationCommandCatalogEntry {
    pub canonical: String,
    pub aliases: Vec<String>,
    pub label: String,
    pub description: String,
    #[serde(default)]
    pub argument_hint: Option<String>,
    pub completes_with_space: bool,
    #[serde(default)]
    pub client_action: Option<DesktopApplicationClientAction>,
    pub available: bool,
    #[serde(default)]
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopApplicationSkillBinding {
    pub skill_id: String,
    pub skill_sha256: String,
    pub index_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopApplicationAgentBinding {
    pub profile_id: String,
    pub snapshot_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopApplicationSkillCatalogEntry {
    pub id: String,
    pub invocation_token: String,
    pub name: String,
    pub description: String,
    pub source: String,
    pub run_mode: String,
    pub trust: String,
    pub available: bool,
    #[serde(default)]
    pub unavailable_reason: Option<String>,
    #[serde(default)]
    pub binding: Option<DesktopApplicationSkillBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopApplicationAgentCatalogEntry {
    pub id: String,
    pub invocation_token: String,
    pub description: String,
    pub source: String,
    pub kind: String,
    pub trust: String,
    pub enabled: bool,
    pub user_invocable: bool,
    pub available: bool,
    #[serde(default)]
    pub unavailable_reason: Option<String>,
    #[serde(default)]
    pub snapshot_id: Option<String>,
    #[serde(default)]
    pub binding: Option<DesktopApplicationAgentBinding>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopApplicationExtensionCatalog {
    pub commands: Vec<DesktopApplicationCommandCatalogEntry>,
    pub skills: Vec<DesktopApplicationSkillCatalogEntry>,
    pub agents: Vec<DesktopApplicationAgentCatalogEntry>,
}

/// Reasoning effort supported by one exact provider/model capability binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopReasoningEffort {
    Low,
    Medium,
    High,
    Max,
}

/// Exact reasoning-effort capabilities for one selectable model.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopApplicationModelOption {
    pub model_name: String,
    pub available_reasoning_efforts: Vec<DesktopReasoningEffort>,
    #[serde(default)]
    pub default_reasoning_effort: Option<DesktopReasoningEffort>,
    #[serde(default)]
    pub reasoning_effort_binding: Option<String>,
}

/// Typed model, permission-mode, and context usage facts for one bound session.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopRunContextView {
    pub provider_name: String,
    pub model_name: String,
    pub available_models: Vec<String>,
    pub model_options: Vec<DesktopApplicationModelOption>,
    pub model_selection: DesktopModelSelectionPolicy,
    pub model_selection_binding: String,
    pub default_permission_mode: DesktopPermissionMode,
    pub available_permission_modes: Vec<DesktopPermissionMode>,
    pub available_reasoning_efforts: Vec<DesktopReasoningEffort>,
    #[serde(default)]
    pub default_reasoning_effort: Option<DesktopReasoningEffort>,
    #[serde(default)]
    pub reasoning_effort_binding: Option<String>,
    #[serde(default)]
    pub context_window_tokens: Option<u32>,
    #[serde(default)]
    pub last_prompt_tokens: Option<u64>,
    pub context_window_source: DesktopContextWindowSource,
    pub extension_catalog: DesktopApplicationExtensionCatalog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopAgentActivityStatus {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopAgentHandoffStatus {
    Pending,
    ResultReady,
    ResultRead,
    Returned,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopAgentUsageSummary {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    #[serde(default)]
    pub cached_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopAgentActivityItem {
    pub thread_id: String,
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    pub objective: String,
    pub status: DesktopAgentActivityStatus,
    #[serde(default)]
    pub reason: Option<String>,
    pub handoff_status: DesktopAgentHandoffStatus,
    #[serde(default)]
    pub result_summary: Option<String>,
    pub result_summary_truncated: bool,
    #[serde(default)]
    pub usage: Option<DesktopAgentUsageSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopAgentActivityView {
    pub total_agents: usize,
    pub active_agents: usize,
    pub terminal_agents: usize,
    pub items: Vec<DesktopAgentActivityItem>,
}

/// Request payload for starting one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopRunStartRequest {
    pub prompt: String,
    pub permission_mode: DesktopPermissionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_selection_binding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<DesktopReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort_binding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_binding: Option<DesktopApplicationSkillBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_binding: Option<DesktopApplicationAgentBinding>,
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
    pub permission_mode: DesktopPermissionMode,
    #[serde(default)]
    pub reasoning_effort: Option<DesktopReasoningEffort>,
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
    #[serde(default)]
    pub foreground_owner: Option<DesktopForegroundRunOwner>,
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
    #[serde(default)]
    pub session_grant_available: bool,
}

/// Explicit user decision for one pending tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopApprovalDecision {
    Approve,
    ApproveForSession,
    Deny,
}

/// Persisted approval outcome returned in a command receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopApprovalRecordedDecision {
    Approved,
    ApprovedForSession,
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
