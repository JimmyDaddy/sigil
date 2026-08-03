use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Deserializer, Serialize};

mod intent_stack;
pub use intent_stack::*;

/// Current command-envelope protocol accepted by `sigil serve`.
pub const DESKTOP_HTTP_PROTOCOL_VERSION: u16 = 2;
pub(crate) const DESKTOP_CONVERSATION_DISPLAY_SCHEMA_VERSION: u16 = 1;
pub(crate) const DESKTOP_CONVERSATION_QUEUE_SCHEMA_VERSION: u16 = 1;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopProviderConfigMode {
    V2,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopProviderModelRef {
    pub connection_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopProviderCredentialSource {
    Environment,
    Stored,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopProviderConnectionReadiness {
    Ready,
    NeedsCredential,
    CredentialUnavailable,
    NeedsModel,
    Unverified,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopProviderConnectionIssue {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopProviderConnectionEntry {
    pub id: String,
    pub label: String,
    pub provider_label: String,
    pub protocol_label: String,
    pub endpoint_display: String,
    pub credential_source: DesktopProviderCredentialSource,
    pub readiness: DesktopProviderConnectionReadiness,
    #[serde(default)]
    pub model_context_windows: BTreeMap<String, u32>,
    #[serde(default)]
    pub default_model: Option<DesktopProviderModelRef>,
    #[serde(default)]
    pub issue: Option<DesktopProviderConnectionIssue>,
}

/// Secret-free provider settings projection owned by native Rust code.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopProviderConnectionInventory {
    pub config_mode: DesktopProviderConfigMode,
    #[serde(default)]
    pub default_model: Option<DesktopProviderModelRef>,
    pub connections: Vec<DesktopProviderConnectionEntry>,
    pub issues: Vec<DesktopProviderConnectionIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopProviderSetupTemplate {
    DeepSeek,
    OpenAi,
    Anthropic,
    Gemini,
    OpenAiCompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopProviderSetupCredentialSource {
    Environment,
    SecureStore,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopProviderSetupProtocol {
    Responses,
    ChatCompletions,
}

/// Secret-bearing model-catalog request admitted only by the native desktop boundary.
///
/// Deliberately does not implement `Debug`, `Clone`, or `Deserialize`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopProviderSetupCatalogRequest {
    pub template: DesktopProviderSetupTemplate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<DesktopProviderSetupProtocol>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub credential_source: DesktopProviderSetupCredentialSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    pub replace_invalid_config: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopProviderSetupModel {
    pub model_id: String,
    pub display_name: String,
    pub availability: String,
    pub recommended: bool,
    pub provenance: String,
    #[serde(default)]
    pub context_window_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopProviderSetupCatalog {
    pub connection_id: String,
    pub provider_label: String,
    pub state: String,
    pub models: Vec<DesktopProviderSetupModel>,
    #[serde(default)]
    pub suggested_model: Option<String>,
    pub manual_entry_allowed: bool,
}

/// Secret-bearing atomic setup request admitted only by the native desktop boundary.
///
/// Deliberately does not implement `Debug`, `Clone`, or `Deserialize`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopProviderSetupSaveRequest {
    pub template: DesktopProviderSetupTemplate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<DesktopProviderSetupProtocol>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub credential_source: DesktopProviderSetupCredentialSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    pub model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub replace_invalid_config: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopProviderSetupSaveResult {
    pub default_model: DesktopProviderModelRef,
    pub inventory: DesktopProviderConnectionInventory,
    pub save_warning: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopProviderDefaultModelSaveRequest {
    pub model_ref: DesktopProviderModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopProviderDefaultModelSaveResult {
    pub default_model: DesktopProviderModelRef,
    pub inventory: DesktopProviderConnectionInventory,
    pub save_warning: bool,
}

/// Request body for creating one process-local session handle.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(default, rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionCreateRequest {
    /// Optional user-visible label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Optional exact connection/model identity for the new durable session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_ref: Option<DesktopProviderModelRef>,
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

/// Exact unavailable source fingerprint selected for native-shell quarantine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionQuarantineRequest {
    pub session_ref: String,
    pub source_bytes: u64,
    pub source_modified_at_unix_ms: u64,
}

/// Exact unavailable source fingerprint selected for native-shell permanent deletion.
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

/// Bounded receipt for one unavailable source moved out of the active catalog.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionQuarantineReceipt {
    pub session_ref: String,
    pub operation_id: String,
    pub quarantine_name: String,
    #[serde(default)]
    pub projection_generation: Option<u64>,
}

/// Bounded receipt for one unavailable source permanently removed from the active catalog.
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
    pub retained_terminal_runs: Vec<DesktopRunSnapshot>,
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
            .field("retained_terminal_runs", &self.retained_terminal_runs.len())
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

/// Durable order for one canonical conversation display item.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationDisplayOrder {
    #[serde(deserialize_with = "deserialize_decimal_u64")]
    pub session_stream_sequence: String,
    pub subindex: u32,
}

/// Provider-neutral visual category for one canonical conversation display item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConversationDisplayItemKind {
    UserMessage,
    Reasoning,
    AssistantMessage,
    Tool,
    Approval,
    Checkpoint,
    Notice,
    Terminal,
}

/// Durable evidence class behind one canonical conversation display item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConversationDisplaySource {
    DurableTranscript,
    DurableRunEvent,
    LiveTransient,
}

/// Bounded lifecycle status for one canonical conversation display item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConversationDisplayStatus {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConversationDisplayMessageRole {
    User,
    Assistant,
}

/// Assistant phase retained for canonical renderer presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConversationDisplayAssistantPhase {
    ToolPreamble,
    Progress,
    FinalAnswer,
}

/// User-selected skill bound to one durable prompt.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationDisplaySkillReference {
    pub id: String,
    pub name: String,
}

/// User decision recorded for one approval item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConversationDisplayApprovalDecision {
    Approved,
    ApprovedForSession,
    Denied,
}

/// Durable checkpoint outcome shown by the canonical renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConversationDisplayCheckpointOutcome {
    Restored,
    Conflict,
}

/// Bounded checkpoint conflict vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConversationDisplayCheckpointConflictReason {
    WorkspaceMismatch,
    CurrentHashMismatch,
    IntentStateConflict,
    ArtifactUnavailable,
    SensitiveSnapshot,
    UnsupportedSnapshot,
    InvalidBinding,
}

/// Typed, secret-safe content carried by one canonical conversation display item.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", deny_unknown_fields)]
pub enum DesktopConversationDisplayContent {
    Message {
        role: DesktopConversationDisplayMessageRole,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        skill: Option<DesktopConversationDisplaySkillReference>,
        #[serde(default)]
        assistant_phase: Option<DesktopConversationDisplayAssistantPhase>,
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
        #[serde(default)]
        call_id: Option<String>,
        #[serde(default)]
        tool_name: Option<String>,
        #[serde(default)]
        output: Option<String>,
        truncated: bool,
        original_content_bytes: u64,
        #[serde(default)]
        artifact_ref: Option<String>,
        #[serde(default)]
        artifact_availability: Option<DesktopToolArtifactAvailability>,
        #[serde(default)]
        observed_bytes: Option<u64>,
        #[serde(default)]
        persisted_bytes: Option<u64>,
        #[serde(default)]
        has_more: bool,
    },
    Approval {
        call_id: String,
        tool_name: String,
        #[serde(default)]
        decision: Option<DesktopConversationDisplayApprovalDecision>,
    },
    Checkpoint {
        outcome: DesktopConversationDisplayCheckpointOutcome,
        #[serde(default)]
        checkpoint_id: Option<String>,
        #[serde(default)]
        conflict_reason: Option<DesktopConversationDisplayCheckpointConflictReason>,
    },
    Notice {
        text: String,
        truncated: bool,
        original_content_bytes: u64,
    },
    Terminal {
        #[serde(default)]
        final_message_id: Option<String>,
        #[serde(default)]
        safe_summary: Option<String>,
        summary_truncated: bool,
    },
}

/// One canonical durable conversation item returned by the workspace server.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationDisplayItem {
    pub schema_version: u16,
    pub display_id: String,
    pub display_order: DesktopConversationDisplayOrder,
    pub source_event_id: String,
    pub kind: DesktopConversationDisplayItemKind,
    pub source: DesktopConversationDisplaySource,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_decimal_u64")]
    pub run_sequence: Option<String>,
    pub status: DesktopConversationDisplayStatus,
    pub content: DesktopConversationDisplayContent,
    #[serde(default)]
    pub reconciles: Option<Vec<String>>,
}

/// Latest proven terminal boundary at a canonical page's durable frontier.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationTerminalFrontier {
    pub run_id: String,
    #[serde(deserialize_with = "deserialize_decimal_u64")]
    pub session_stream_sequence: String,
    pub status: DesktopConversationDisplayStatus,
}

/// Bounded canonical display gap vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConversationDisplayGapKind {
    Retention,
    Replay,
}

/// Gap fact retained without exposing journal or filesystem details.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationDisplayGapFact {
    pub kind: DesktopConversationDisplayGapKind,
    #[serde(deserialize_with = "deserialize_decimal_u64")]
    pub after_session_stream_sequence: String,
}

/// Process-local run anchor observed after a durable page was projected.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationLiveProvisionalAnchor {
    #[serde(deserialize_with = "deserialize_decimal_u64")]
    pub durable_frontier: String,
    pub run_id: String,
    #[serde(deserialize_with = "deserialize_decimal_u64")]
    pub run_sequence: String,
}

/// Bounded durable plan-step state used to restore Task controls.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationTaskPlanStep {
    pub step_id: String,
    pub title: String,
    pub role: String,
    pub depends_on: Vec<String>,
    pub mode: String,
    pub isolation: String,
    #[serde(default)]
    pub status: Option<String>,
}

/// Bounded durable integration-lane state without private physical identities.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationTaskLane {
    pub lane_id: String,
    #[serde(default)]
    pub plan_id: Option<String>,
    pub status: String,
    #[serde(default)]
    pub conflicts: Vec<String>,
}

/// Current durable Task control state at the canonical display frontier.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationTaskControl {
    pub schema_version: u16,
    pub task_id: String,
    pub phase: crate::DesktopPublicTaskPhase,
    pub status: String,
    #[serde(default)]
    pub plan_version: Option<u32>,
    #[serde(default)]
    pub plan_status: Option<String>,
    pub steps: Vec<DesktopConversationTaskPlanStep>,
    pub steps_truncated: bool,
    pub active_children: u32,
    pub completed_children: u32,
    pub failed_children: u32,
    pub lanes: Vec<DesktopConversationTaskLane>,
    pub lanes_truncated: bool,
    pub can_continue: bool,
}

/// Opaque-cursor page over canonical durable conversation display items.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationDisplayPage {
    pub schema_version: u16,
    /// Process-local adapter session id; no durable scope is exposed.
    pub request_scope: String,
    #[serde(deserialize_with = "deserialize_decimal_u64")]
    pub through_session_stream_sequence: String,
    #[serde(default)]
    pub terminal_frontier: Option<DesktopConversationTerminalFrontier>,
    #[serde(deserialize_with = "deserialize_decimal_u64")]
    pub total_items: String,
    pub items: Vec<DesktopConversationDisplayItem>,
    #[serde(default, deserialize_with = "deserialize_optional_opaque_cursor")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
    #[serde(default)]
    pub gap_facts: Vec<DesktopConversationDisplayGapFact>,
    #[serde(default)]
    pub live_provisional_anchor: Option<DesktopConversationLiveProvisionalAnchor>,
    #[serde(default)]
    pub task_control: Option<DesktopConversationTaskControl>,
}

/// Bounded query for one canonical conversation display page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopConversationDisplayQuery {
    pub cursor: Option<String>,
    pub limit: Option<u16>,
}

/// Typed display availability for one durable tool artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopToolArtifactAvailability {
    Available,
    Expired,
    Missing,
    HashMismatch,
    PolicyRevoked,
    Unavailable,
}

/// Typed, bounded selector accepted by the display artifact endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum DesktopToolArtifactSelector {
    ByteSlice {
        offset: u64,
        limit: u32,
    },
    LinePage {
        start_line: u64,
        line_count: u32,
    },
    SearchLiteral {
        query: String,
        start_offset: u64,
        max_matches: u16,
        context_lines: u16,
    },
}

/// Narrow request for one session-scoped artifact page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct DesktopToolArtifactReadRequest {
    pub artifact_ref: String,
    pub selector: DesktopToolArtifactSelector,
}

/// Encoding of one bounded artifact body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopToolArtifactPageEncoding {
    Utf8,
    Base64,
}

/// One typed, bounded artifact page from the authenticated workspace server.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopToolArtifactPage {
    pub schema_version: u16,
    pub request_scope: String,
    pub artifact_ref: String,
    pub selector: DesktopToolArtifactSelector,
    pub body: String,
    pub body_encoding: DesktopToolArtifactPageEncoding,
    pub returned_bytes: u32,
    pub page_sha256: String,
    pub artifact_sha256: String,
    pub eof: bool,
    pub match_count: u16,
    #[serde(default)]
    pub next_selector: Option<DesktopToolArtifactSelector>,
}

fn deserialize_decimal_u64<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    validate_decimal_u64(value).map_err(serde::de::Error::custom)
}

fn deserialize_optional_decimal_u64<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(validate_decimal_u64)
        .transpose()
        .map_err(serde::de::Error::custom)
}

fn validate_decimal_u64(value: String) -> Result<String, &'static str> {
    if value.is_empty()
        || value.len() > 20
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
        || value.parse::<u64>().is_err()
    {
        return Err("expected canonical decimal u64 text");
    }
    Ok(value)
}

fn deserialize_optional_opaque_cursor<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let cursor = Option::<String>::deserialize(deserializer)?;
    if cursor.as_deref().is_some_and(|value| {
        value.is_empty() || value.len() > 4_096 || value.chars().any(char::is_control)
    }) {
        return Err(serde::de::Error::custom("invalid opaque display cursor"));
    }
    Ok(cursor)
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
    /// The durable source is malformed or inconsistent.
    Invalid,
}

/// Path-free explanation for an invalid historical catalog source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopSessionCatalogSourceDiagnostic {
    UnsafeSource,
    InvalidEventStream,
    InvalidProjection,
    MissingSessionIdentity,
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
    #[serde(default)]
    pub source_diagnostic: Option<DesktopSessionCatalogSourceDiagnostic>,
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
    SameSession,
}

/// Evidence source used to resolve a session context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopContextWindowSource {
    Connection,
    Provider,
    Config,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopApplicationClientAction {
    PreviewCompaction,
    OpenIntentStack,
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
    pub model_ref: DesktopProviderModelRef,
    pub display_name: String,
    pub availability: String,
    pub recommendation: String,
    pub provenance: String,
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
    pub model_ref: DesktopProviderModelRef,
    pub provider_name: String,
    pub model_name: String,
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
    pub model_ref: Option<DesktopProviderModelRef>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_continuation: Option<DesktopTaskContinuationRequest>,
}

/// Exact durable Task continuation requested instead of a new conversation turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopTaskContinuationRequest {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
}

/// Request payload for cooperative cancellation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(default, rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopRunCancelRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Exact generation-bound request for stopping one persistent terminal task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopTerminalTaskCancelRequest {
    pub task_id: String,
    pub expected_generation: u64,
}

/// Exact durable Task pause payload constructed by the native client boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopTaskPauseRequest {
    pub request_id: String,
    pub task_id: String,
    pub plan_version: u32,
}

/// Public run lifecycle returned by the HTTP adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopRunStatus {
    Starting,
    Running,
    WaitingForApproval,
    CancelRequested,
    PauseRequested,
    ExecutionUncertain,
    Finished,
    Failed,
    Cancelled,
    Paused,
    Interrupted,
}

/// Canonical non-pending approval lifecycle state returned by the workspace server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopApprovalLifecycleState {
    Resolving,
    DecisionAccepted,
    Resolved,
    ExecutionStarted,
    DeliveryUncertain,
    Terminal,
}

/// Exact bounded approval identity used to recover renderer state after an event gap.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopApprovalLifecycleView {
    pub approval: DesktopPendingApproval,
    pub state: DesktopApprovalLifecycleState,
}

impl DesktopRunStatus {
    /// Returns whether command routing has reached a terminal state.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Finished | Self::Failed | Self::Cancelled | Self::Paused | Self::Interrupted
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
    pub pending_approvals: Vec<DesktopPendingApproval>,
    #[serde(default)]
    pub approval_lifecycles: Vec<DesktopApprovalLifecycleView>,
    #[serde(default)]
    pub terminal_tasks: Vec<DesktopTerminalLifecycleView>,
    pub stream_sequence: u64,
}

/// Typed bounded terminal owner snapshot returned with a run.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopTerminalLifecycleView {
    pub task_id: String,
    #[serde(default)]
    pub execution_backend: Option<DesktopTerminalExecutionBackendKind>,
    #[serde(default)]
    pub sandbox_profile: Option<DesktopExecutionSandboxProfile>,
    pub generation: u64,
    pub status: DesktopTerminalTaskStatus,
    pub readiness: DesktopTerminalReadinessStatus,
    pub total_output_bytes: u64,
    pub emitted_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopTerminalExecutionBackendKind {
    LocalProcess,
    LocalPty,
    SandboxedPty,
}

impl DesktopTerminalExecutionBackendKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalProcess => "local_process",
            Self::LocalPty => "local_pty",
            Self::SandboxedPty => "sandboxed_pty",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopExecutionSandboxProfile {
    Unconfined,
    WorkspaceWrite,
    BuildOffline,
    BuildNetworked,
}

impl DesktopExecutionSandboxProfile {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unconfined => "unconfined",
            Self::WorkspaceWrite => "workspace_write",
            Self::BuildOffline => "build_offline",
            Self::BuildNetworked => "build_networked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DesktopTerminalTaskStatus {
    Starting,
    Running,
    Exited {
        #[serde(default)]
        exit_code: Option<i32>,
    },
    Failed {
        reason: String,
    },
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopTerminalReadinessKind {
    None,
    OutputContains,
    OutputRegex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DesktopTerminalReadinessStatus {
    None,
    Waiting {
        kind: DesktopTerminalReadinessKind,
    },
    Ready {
        kind: DesktopTerminalReadinessKind,
        ready_at_ms: u64,
    },
    Failed {
        kind: DesktopTerminalReadinessKind,
        reason: String,
    },
    TimedOut {
        kind: DesktopTerminalReadinessKind,
    },
}

/// Opaque compare-and-swap generation for one exact durable queue projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DesktopConversationQueueGeneration(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConversationQueueItemKind {
    Chat,
    PlanPrompt,
    AgentMention,
    AgentMessage,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConversationQueueItemStatus {
    Queued,
    Dispatching,
    Delivered,
    Rejected,
    Cancelled,
    Stale,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConversationQueuePromptMaterial {
    PersistedSafe,
    AvailableProcessLocal,
    RequiresReentry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConversationQueueBlockedReason {
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

/// One secret-free queue row. Exact prompts and prompt hashes stay behind the server boundary.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationQueueItem {
    pub entry_id: String,
    pub order: u32,
    pub kind: DesktopConversationQueueItemKind,
    pub status: DesktopConversationQueueItemStatus,
    pub prompt_preview: String,
    pub prompt_preview_truncated: bool,
    pub prompt_material: DesktopConversationQueuePromptMaterial,
    pub dispatchable: bool,
    #[serde(default)]
    pub blocked_reason: Option<DesktopConversationQueueBlockedReason>,
    #[serde(default)]
    pub created_at_ms: Option<u64>,
    #[serde(default)]
    pub updated_at_ms: Option<u64>,
}

/// Bounded queue projection for one exact desktop session handle.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationQueueView {
    pub schema_version: u16,
    pub session_id: String,
    pub generation: DesktopConversationQueueGeneration,
    pub paused: bool,
    pub total_items: u32,
    pub items: Vec<DesktopConversationQueueItem>,
    pub truncated: bool,
    #[serde(default)]
    pub next_dispatchable_entry_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConversationQueueCommandActionKind {
    Enqueue,
    Edit,
    Remove,
    Reorder,
    Pause,
    Resume,
    InterruptAndRunNext,
}

/// Exact queue mutation. Prompts are request-only and are never present in a receipt or view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopConversationQueueCommandAction {
    Enqueue {
        prompt: String,
        kind: DesktopConversationQueueItemKind,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<DesktopReasoningEffort>,
    },
    Edit {
        entry_id: String,
        prompt: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<DesktopReasoningEffort>,
    },
    Remove {
        entry_id: String,
    },
    Reorder {
        entry_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        after_entry_id: Option<String>,
    },
    Pause,
    Resume,
    InterruptAndRunNext {
        foreground_run_id: String,
        foreground_owner_revision: String,
    },
}

impl DesktopConversationQueueCommandAction {
    #[must_use]
    pub const fn kind(&self) -> DesktopConversationQueueCommandActionKind {
        match self {
            Self::Enqueue { .. } => DesktopConversationQueueCommandActionKind::Enqueue,
            Self::Edit { .. } => DesktopConversationQueueCommandActionKind::Edit,
            Self::Remove { .. } => DesktopConversationQueueCommandActionKind::Remove,
            Self::Reorder { .. } => DesktopConversationQueueCommandActionKind::Reorder,
            Self::Pause => DesktopConversationQueueCommandActionKind::Pause,
            Self::Resume => DesktopConversationQueueCommandActionKind::Resume,
            Self::InterruptAndRunNext { .. } => {
                DesktopConversationQueueCommandActionKind::InterruptAndRunNext
            }
        }
    }
}

/// Queue-specific compare-and-swap payload carried by the generic desktop command envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationQueueCommandRequest {
    pub expected_generation: DesktopConversationQueueGeneration,
    pub action: DesktopConversationQueueCommandAction,
}

/// Durable queue mutation receipt with no exact prompt material.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationQueueCommandReceipt {
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    pub action: DesktopConversationQueueCommandActionKind,
    pub expected_generation: DesktopConversationQueueGeneration,
    pub generation: DesktopConversationQueueGeneration,
    #[serde(default)]
    pub interrupt_owner: Option<DesktopForegroundRunOwner>,
    pub queue: DesktopConversationQueueView,
    #[serde(default)]
    pub correlation_id: Option<String>,
    pub replayed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopCheckpointRestoreKind {
    RestoreContent,
    RemoveCreatedFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopCheckpointFileAvailability {
    Restorable,
    Sensitive,
    Unsupported,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopCheckpointFileView {
    pub path: String,
    pub restore_kind: DesktopCheckpointRestoreKind,
    pub availability: DesktopCheckpointFileAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopCheckpointView {
    pub checkpoint_id: String,
    pub checkpoint_digest: String,
    pub turn_index: usize,
    #[serde(default)]
    pub prompt: Option<String>,
    pub files: Vec<DesktopCheckpointFileView>,
    pub unknown_mutation_count: usize,
    pub fully_restorable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationForkPointView {
    pub source_turn_index: usize,
    pub source_turn_digest: String,
    pub source_boundary_stream_sequence: u64,
    pub source_finalized_stream_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationRecoveryView {
    pub checkpoints: Vec<DesktopCheckpointView>,
    pub fork_points: Vec<DesktopConversationForkPointView>,
    pub through_stream_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopCompactionEconomics {
    pub before_input_tokens: u64,
    pub target_input_tokens: u64,
    pub context_window_tokens: u64,
    pub output_tokens: u64,
    pub safety_buffer_tokens: u64,
    pub savings_tokens: u64,
    pub savings_ratio_ppm: u32,
    pub minimum_savings_tokens: u64,
    pub minimum_savings_ratio_ppm: u32,
    pub summary_cache_read_tokens: u64,
    pub summary_uncached_input_tokens: u64,
    pub summary_output_tokens: u64,
    #[serde(default)]
    pub summary_cost_nano_usd: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum DesktopCompactionAdmission {
    Prepared {
        standalone_tool_output_shrink_available: bool,
    },
    Ready {
        economics: DesktopCompactionEconomics,
    },
    NoFoldableHistory {
        durable_message_count: usize,
        minimum_tail_turn_count: usize,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopCompactionPolicy {
    pub strategy: String,
    pub phase: String,
    #[serde(default)]
    pub forecast_confidence: Option<String>,
    #[serde(default)]
    pub admission_reason: Option<String>,
    pub native_carrier_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopCompactionConstraint {
    pub text: String,
    pub source_event_id: String,
    pub source_field_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopCompactionToolArtifact {
    pub source_event_id: String,
    pub content_sha256: String,
    pub tool_name: String,
    pub tool_call_id: String,
    pub status: String,
    pub original_content_bytes: u64,
    pub original_content_token_upper_bound: u64,
    pub head_excerpt: String,
    pub tail_excerpt: String,
    pub reason: String,
    pub recovery_instruction: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopCompactionDetails {
    pub active_objective: String,
    pub objective_source_event_id: String,
    pub active_constraints: Vec<DesktopCompactionConstraint>,
    pub folded_complete_turn_count: usize,
    pub folded_token_upper_bound: u64,
    pub retained_complete_turn_count: usize,
    pub retained_token_upper_bound: u64,
    pub tool_artifact_count: usize,
    #[serde(default)]
    pub tool_artifacts: Vec<DesktopCompactionToolArtifact>,
    pub pending_work_count: usize,
    pub unresolved_question_count: usize,
    pub recoverable_attachment_count: usize,
    pub protected_control_event_count: usize,
    pub protected_active_tool_or_approval_count: usize,
    #[serde(default)]
    pub current_cache_read_tokens: Option<u64>,
    #[serde(default)]
    pub break_even_turns: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopCompactionReview {
    #[serde(default)]
    pub preview_id: Option<String>,
    pub folded_event_count: usize,
    pub retained_event_count: usize,
    #[serde(default)]
    pub policy: Option<DesktopCompactionPolicy>,
    #[serde(default)]
    pub details: Option<DesktopCompactionDetails>,
    pub admission: DesktopCompactionAdmission,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopCheckpointRestoreRequest {
    pub checkpoint_id: String,
    pub checkpoint_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopCheckpointRestoreConflictReason {
    WorkspaceMismatch,
    CurrentHashMismatch,
    IntentStateConflict,
    ArtifactUnavailable,
    SensitiveSnapshot,
    UnsupportedSnapshot,
    InvalidBinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopCheckpointRestorePreviewFile {
    pub path: String,
    pub restore_kind: DesktopCheckpointRestoreKind,
    #[serde(default)]
    pub expected_current_hash: Option<String>,
    #[serde(default)]
    pub actual_current_hash: Option<String>,
    #[serde(default)]
    pub conflict_reason: Option<DesktopCheckpointRestoreConflictReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopCheckpointReverseDiff {
    pub path: String,
    pub diff: String,
    pub truncated: bool,
    pub original_line_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopCheckpointRestoreReview {
    pub checkpoint_id: String,
    pub checkpoint_digest: String,
    pub files: Vec<DesktopCheckpointRestorePreviewFile>,
    pub reverse_diffs: Vec<DesktopCheckpointReverseDiff>,
    pub unknown_mutation_count: usize,
    pub ready: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConversationRecoveryCommandActionKind {
    PrepareCompaction,
    ApplyCompaction,
    ApplyStandaloneToolOutputShrink,
    RestoreCheckpoint,
    ForkConversation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopConversationRecoveryCommandAction {
    PrepareCompaction {
        preview_id: String,
    },
    ApplyCompaction {
        preview_id: String,
    },
    ApplyStandaloneToolOutputShrink {
        preview_id: String,
    },
    RestoreCheckpoint {
        checkpoint_id: String,
        checkpoint_digest: String,
    },
    ForkConversation {
        source_turn_digest: String,
        model_ref: DesktopProviderModelRef,
    },
}

impl DesktopConversationRecoveryCommandAction {
    #[must_use]
    pub const fn kind(&self) -> DesktopConversationRecoveryCommandActionKind {
        match self {
            Self::PrepareCompaction { .. } => {
                DesktopConversationRecoveryCommandActionKind::PrepareCompaction
            }
            Self::ApplyCompaction { .. } => {
                DesktopConversationRecoveryCommandActionKind::ApplyCompaction
            }
            Self::ApplyStandaloneToolOutputShrink { .. } => {
                DesktopConversationRecoveryCommandActionKind::ApplyStandaloneToolOutputShrink
            }
            Self::RestoreCheckpoint { .. } => {
                DesktopConversationRecoveryCommandActionKind::RestoreCheckpoint
            }
            Self::ForkConversation { .. } => {
                DesktopConversationRecoveryCommandActionKind::ForkConversation
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopCompactionReceipt {
    pub compaction_id: String,
    pub attempt_id: String,
    pub task_memory_id: String,
    pub folded_event_count: usize,
    pub tool_output_projection_recorded: bool,
    pub native_carrier_materialized: bool,
    #[serde(default)]
    pub native_carrier_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopToolOutputShrinkReceipt {
    pub context_epoch_id: String,
    pub projected_output_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopCheckpointRestoreReceipt {
    pub checkpoint_id: String,
    pub batch_id: String,
    pub restored_file_count: usize,
    pub verification_stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationForkReceipt {
    pub session_ref: String,
    pub session_id: String,
    pub copied_message_count: usize,
    pub copied_external_provenance_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopConversationRecoveryCommandReceipt {
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    pub action: DesktopConversationRecoveryCommandActionKind,
    #[serde(default)]
    pub compaction: Option<DesktopCompactionReceipt>,
    #[serde(default)]
    pub compaction_review: Option<DesktopCompactionReview>,
    #[serde(default)]
    pub tool_output_shrink: Option<DesktopToolOutputShrinkReceipt>,
    #[serde(default)]
    pub restore: Option<DesktopCheckpointRestoreReceipt>,
    #[serde(default)]
    pub fork: Option<DesktopConversationForkReceipt>,
    pub recovery: DesktopConversationRecoveryView,
    #[serde(default)]
    pub correlation_id: Option<String>,
    pub replayed: bool,
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

/// Receipt from pausing one exact durable Task.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopTaskPauseCommandReceipt {
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    #[serde(default)]
    pub expected_stream_sequence: Option<u64>,
    #[serde(default)]
    pub correlation_id: Option<String>,
    pub task_id: String,
    pub plan_version: u32,
    pub run: DesktopRunSnapshot,
    pub replayed: bool,
}

/// Receipt from stopping one exact persistent terminal task.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopTerminalTaskCancelCommandReceipt {
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    #[serde(default)]
    pub expected_stream_sequence: Option<u64>,
    #[serde(default)]
    pub correlation_id: Option<String>,
    pub run_id: String,
    pub terminal_task: DesktopTerminalLifecycleView,
    pub replayed: bool,
}

/// Renderer-safe mirror of the shared session-grant unavailability reason code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopSessionGrantUnavailableReasonCode {
    AnalysisIncomplete,
    SemanticScopeUnavailable,
    NonGrantableEffect,
    ContainmentBindingUnavailable,
    PolicyDecisionNotGrantable,
    NoReusableApprovalFacet,
    NetworkScopeNotGrantable,
    ConfirmationRequired,
    SnapshotRequired,
    SubjectScopeUnavailable,
    RiskNotGrantable,
    ExternalMutation,
    OperationNotGrantable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopSessionGrantUnavailableReason {
    pub code: DesktopSessionGrantUnavailableReasonCode,
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
    pub session_grant_available: bool,
    pub session_grant_unavailable_reason: Option<DesktopSessionGrantUnavailableReason>,
    pub display: DesktopPendingApprovalDisplay,
}

/// Bounded, credential-free facts used to rebuild one pending approval from a run snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopPendingApprovalDisplay {
    pub event_sequence: u64,
    #[serde(default)]
    pub effects: Vec<String>,
    #[serde(default)]
    pub subjects: Vec<DesktopPendingApprovalSubject>,
    pub analysis_status: String,
    #[serde(default)]
    pub analysis_reason_codes: Vec<String>,
    #[serde(default)]
    pub analysis_reasons: Vec<String>,
    #[serde(default)]
    pub containment: Vec<String>,
    #[serde(default)]
    pub decision_reasons: Vec<String>,
    pub safe_summary_title: String,
    pub safe_summary_detail: String,
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub risk: Option<String>,
    #[serde(default)]
    pub snapshot_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopPendingApprovalSubject {
    pub kind: String,
    pub scope: String,
    #[serde(default)]
    pub workspace_label: Option<String>,
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

/// Server-owned routing state for one exact approval command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopApprovalRouteState {
    DecisionAccepted,
    DeliveryUncertain,
    Terminal,
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
    pub approval_request_id: String,
    #[serde(default)]
    pub expected_stream_sequence: Option<u64>,
    #[serde(default)]
    pub correlation_id: Option<String>,
    pub decision: DesktopApprovalDecisionRecord,
    pub route_state: DesktopApprovalRouteState,
    pub registry_revision: u64,
    pub replayed: bool,
}

/// Exact stale-safe binding for one recommended task verification check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopVerificationRerunRequest {
    pub request_id: String,
    pub task_id: String,
    pub plan_version: u32,
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

/// Exact stale-safe identity for one current Task integration review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopTaskIntegrationReviewRequest {
    pub request_id: String,
    pub task_id: String,
    pub plan_id: String,
    pub plan_version: u32,
    pub preview_digest: String,
}

/// Renderer-safe final promotion target kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopIntegrationPromotionTargetKind {
    WorkspaceApply,
    GitRefAdvance,
}

/// Renderer-safe physical integration lane candidate kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopIntegrationLaneCandidateKind {
    ManagedRef,
    SnapshotWorkspace,
}

/// Bounded, private-ref-free provenance for one reviewed integration lane.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopTaskIntegrationLaneView {
    pub lane_id: String,
    pub candidate_kind: DesktopIntegrationLaneCandidateKind,
    pub proposal_count: usize,
    pub verification_receipt_count: usize,
}

/// Exact current Task integration review returned by the local server.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopTaskIntegrationReviewView {
    pub schema_version: u16,
    pub request: DesktopTaskIntegrationReviewRequest,
    pub aggregate_diff: String,
    pub aggregate_diff_digest: String,
    pub preview_digest: String,
    pub policy_digest: String,
    pub target_kind: DesktopIntegrationPromotionTargetKind,
    pub lanes: Vec<DesktopTaskIntegrationLaneView>,
    pub child_verification_receipt_count: usize,
    pub lane_verification_receipt_count: usize,
    pub conflict_reasons: Vec<String>,
    pub verification_invalidation_count: usize,
    pub parent_verification_pending: bool,
}

/// Terminal status of one exact final Task promotion attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopIntegrationPromotionStatus {
    Prepared,
    Promoted,
    Conflict,
    Stale,
    Failed,
    Cancelled,
}

/// Terminal, renderer-safe result of accepting one exact Task integration review.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopTaskIntegrationAcceptanceView {
    pub request: DesktopTaskIntegrationReviewRequest,
    pub promotion_status: DesktopIntegrationPromotionStatus,
    #[serde(default)]
    pub parent_verdict: Option<DesktopVerificationVerdict>,
    pub can_continue: bool,
    #[serde(default)]
    pub promotion_cleanup_error: Option<String>,
    #[serde(default)]
    pub parent_cleanup_error: Option<String>,
}

/// Receipt from accepting one exact Task integration review.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopTaskIntegrationAcceptanceCommandReceipt {
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    #[serde(default)]
    pub correlation_id: Option<String>,
    pub acceptance: DesktopTaskIntegrationAcceptanceView,
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
