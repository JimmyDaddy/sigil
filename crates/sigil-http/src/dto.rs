use std::{fmt, net::SocketAddr};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize};

/// Policy identity bound to every V1 HTTP approval request.
pub const HTTP_APPROVAL_POLICY_VERSION: &str = "sigil-http-approval-v1";
use sigil_kernel::session::TOOL_ARTIFACT_MAX_BYTES;
use sigil_kernel::{
    IntentDropRequestV1, IntentOperationExecutionV1, IntentOperationPreviewV1, IntentVersionRef,
    PublicIntentStackStateV1, PublicTaskPhase, TaskIntegrationReviewRequest, TaskPauseRequest,
    TaskVerificationRerunRequest, ToolApprovalUserDecision, ToolArtifactPageEncoding,
    ToolArtifactPageV1, ToolArtifactRefV1, ToolArtifactSelectorV1, VerificationProductView,
    stable_event_hash,
};
use sigil_runtime::application_compaction::{
    ApplicationCompactionAdmission, ApplicationCompactionDetailsView,
    ApplicationCompactionPolicyView, ApplicationCompactionReview,
};
use sigil_runtime::application_recovery::{
    ApplicationCheckpointRestoreReview, ApplicationConversationRecoveryView,
};
use sigil_runtime::application_run::{
    ApplicationIntegrationLaneCandidateKind, ApplicationIntegrationPromotionTargetKind,
    ApplicationTaskIntegrationAcceptanceView, ApplicationTaskIntegrationReviewView,
};
use sigil_runtime::conversation_display::{
    ConversationDisplayApprovalDecisionV1, ConversationDisplayAssistantPhaseV1,
    ConversationDisplayCheckpointConflictReasonV1, ConversationDisplayCheckpointOutcomeV1,
    ConversationDisplayContentV1, ConversationDisplayItemKindV1, ConversationDisplayItemV1,
    ConversationDisplayMessageRoleV1, ConversationDisplayPageV1, ConversationDisplaySourceV1,
    ConversationDisplayStatusV1, ConversationTaskControlV1, ConversationTaskLaneV1,
    ConversationTaskPlanStepV1, ConversationTerminalFrontierV1,
};
use sigil_runtime::support::{
    DoctorSupportReportV1, SupportDoctorCheckV1, SupportDoctorStatus, SupportEnvironmentV1,
    SupportPrivacyV1, SupportTerminalFamily,
};

/// Schema version for the desktop launcher/server metadata handshake.
pub const HTTP_SERVER_INFO_SCHEMA_VERSION: u16 = 13;
/// Schema version for one bounded display-surface artifact page.
pub const HTTP_TOOL_ARTIFACT_PAGE_SCHEMA_VERSION: u16 = 1;

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
    /// A bound durable session exposes typed, bounded artifact pages by opaque reference.
    pub typed_tool_artifact_retrieval: bool,
    /// Durable run events support cursor-bound replay.
    pub durable_event_replay: bool,
    /// Transient and durable run events can be followed while the server is active.
    pub live_events: bool,
    /// Pending tool approvals can be resolved by an authenticated client.
    pub approval: bool,
    /// Active runs support cooperative cancellation and bounded drain.
    pub cancellation: bool,
    /// Active durable Tasks support exact plan- and scope-bound pause.
    pub task_pause: bool,
    /// Durable task verification can be inspected and one exact recommended check rerun.
    pub verification: bool,
    /// Durable Task integration can be reviewed and one exact preview accepted.
    pub task_integration: bool,
    /// Durable Intent Stack state can be reviewed and one exact Drop preview confirmed.
    pub intent_stack: bool,
    /// Bound sessions expose typed model, permission-mode, and context-usage facts.
    pub run_context: bool,
    /// Bound sessions expose a safe, bounded child-agent lifecycle and handoff projection.
    pub agent_activity: bool,
    /// Bound sessions expose exact compact/checkpoint/fork recovery controls.
    pub conversation_recovery: bool,
    /// Redacted local diagnostics and an explicit private support-bundle export are available.
    pub support_diagnostics: bool,
    /// Secret-free provider connection inventory is available to the native owner.
    pub provider_connections: bool,
    /// Authenticated provider catalog and atomic setup writes are available to the native owner.
    pub provider_setup: bool,
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
            typed_tool_artifact_retrieval: true,
            durable_event_replay: true,
            live_events: true,
            approval: true,
            cancellation: true,
            task_pause: true,
            verification: true,
            task_integration: true,
            intent_stack: true,
            run_context: true,
            agent_activity: true,
            conversation_recovery: true,
            support_diagnostics: true,
            provider_connections: true,
            provider_setup: true,
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

/// Configuration schema mode projected without exposing raw configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpProviderConfigMode {
    V2,
    Invalid,
}

/// Compound connection/model identity used by settings surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpProviderModelRef {
    pub connection_id: String,
    pub model_id: String,
}

/// Secret-free credential source classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpProviderCredentialSource {
    Environment,
    Stored,
    None,
}

/// Native-owner readiness state for one configured connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpProviderConnectionReadiness {
    Ready,
    NeedsCredential,
    CredentialUnavailable,
    NeedsModel,
    Unverified,
    Invalid,
}

/// Bounded, stable issue projection that contains neither paths nor provider response bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpProviderConnectionIssue {
    pub code: String,
    pub message: String,
}

/// One secret-free connection row for a native settings owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpProviderConnectionEntry {
    pub id: String,
    pub label: String,
    pub provider_label: String,
    pub protocol_label: String,
    pub endpoint_display: String,
    pub credential_source: HttpProviderCredentialSource,
    pub readiness: HttpProviderConnectionReadiness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<HttpProviderModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<HttpProviderConnectionIssue>,
}

/// Full secret-free inventory shared by Doctor, TUI runtime ownership, and Desktop native code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpProviderConnectionInventory {
    pub config_mode: HttpProviderConfigMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<HttpProviderModelRef>,
    pub connections: Vec<HttpProviderConnectionEntry>,
    pub issues: Vec<HttpProviderConnectionIssue>,
}

/// Provider templates available to first-run and settings connection wizards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpProviderSetupTemplate {
    DeepSeek,
    OpenAi,
    Anthropic,
    Gemini,
    OpenAiCompatible,
}

/// Authentication source selected by a provider connection wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpProviderSetupCredentialSource {
    Environment,
    SecureStore,
    None,
}

/// Wire protocol choice exposed only for an OpenAI-compatible custom endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpProviderSetupProtocol {
    Responses,
    ChatCompletions,
}

/// Secret-bearing request admitted only by the authenticated native desktop owner.
///
/// Deliberately does not implement `Debug`, `Clone`, or `Serialize`.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpProviderSetupCatalogRequest {
    pub template: HttpProviderSetupTemplate,
    #[serde(default)]
    pub protocol: Option<HttpProviderSetupProtocol>,
    #[serde(default)]
    pub endpoint: Option<String>,
    pub credential_source: HttpProviderSetupCredentialSource,
    #[serde(default)]
    pub api_key: Option<String>,
}

/// Secret-free model row returned by the setup catalog boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpProviderSetupModel {
    pub model_id: String,
    pub display_name: String,
    pub availability: String,
    pub recommended: bool,
    pub provenance: String,
}

/// Exact connection-scoped catalog view used by a first-run or settings wizard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpProviderSetupCatalog {
    pub connection_id: String,
    pub provider_label: String,
    pub state: String,
    pub models: Vec<HttpProviderSetupModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_model: Option<String>,
    pub manual_entry_allowed: bool,
}

/// Secret-bearing atomic setup request. The model always belongs to the generated connection.
///
/// Deliberately does not implement `Debug`, `Clone`, or `Serialize`.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpProviderSetupSaveRequest {
    pub template: HttpProviderSetupTemplate,
    #[serde(default)]
    pub protocol: Option<HttpProviderSetupProtocol>,
    #[serde(default)]
    pub endpoint: Option<String>,
    pub credential_source: HttpProviderSetupCredentialSource,
    #[serde(default)]
    pub api_key: Option<String>,
    pub model_id: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// Secret-free result after atomically publishing one connection and saved default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpProviderSetupSaveResult {
    pub default_model: HttpProviderModelRef,
    pub inventory: HttpProviderConnectionInventory,
    pub save_warning: bool,
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
    /// Optional exact connection/model identity for the new durable session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_ref: Option<HttpProviderModelRef>,
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

/// Exact unavailable catalog source fingerprint selected for quarantine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpSessionQuarantineRequest {
    pub session_ref: String,
    pub source_bytes: u64,
    pub source_modified_at_unix_ms: u64,
}

/// Exact unavailable catalog source fingerprint selected for permanent deletion.
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

/// Bounded receipt for an unavailable source moved out of the active catalog.
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

/// Bounded receipt for one unavailable source permanently removed from the active catalog.
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

/// User-selected skill bound to one durable prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationDisplaySkillReference {
    pub id: String,
    pub name: String,
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
    IntentStateConflict,
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
        skill: Option<HttpConversationDisplaySkillReference>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_availability: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        persisted_bytes: Option<u64>,
        #[serde(default)]
        has_more: bool,
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

/// Bounded durable plan-step state used to restore application Task controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationTaskPlanStep {
    pub step_id: String,
    pub title: String,
    pub role: String,
    pub depends_on: Vec<String>,
    pub mode: String,
    pub isolation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl From<ConversationTaskPlanStepV1> for HttpConversationTaskPlanStep {
    fn from(step: ConversationTaskPlanStepV1) -> Self {
        Self {
            step_id: step.step_id,
            title: step.title,
            role: step.role,
            depends_on: step.depends_on,
            mode: step.mode,
            isolation: step.isolation,
            status: step.status,
        }
    }
}

/// Bounded durable integration-lane state with no private workspace or ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationTaskLane {
    pub lane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    pub status: String,
    #[serde(default)]
    pub conflicts: Vec<String>,
}

impl From<ConversationTaskLaneV1> for HttpConversationTaskLane {
    fn from(lane: ConversationTaskLaneV1) -> Self {
        Self {
            lane_id: lane.lane_id,
            plan_id: lane.plan_id,
            status: lane.status,
            conflicts: lane.conflicts,
        }
    }
}

/// Current durable Task control state at the canonical display frontier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationTaskControl {
    pub schema_version: u16,
    pub task_id: String,
    pub phase: PublicTaskPhase,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_status: Option<String>,
    pub steps: Vec<HttpConversationTaskPlanStep>,
    pub steps_truncated: bool,
    pub active_children: u32,
    pub completed_children: u32,
    pub failed_children: u32,
    pub lanes: Vec<HttpConversationTaskLane>,
    pub lanes_truncated: bool,
    pub can_continue: bool,
}

impl From<ConversationTaskControlV1> for HttpConversationTaskControl {
    fn from(task: ConversationTaskControlV1) -> Self {
        Self {
            schema_version: task.schema_version,
            task_id: task.task_id,
            phase: task.phase,
            status: task.status,
            plan_version: task.plan_version,
            plan_status: task.plan_status,
            steps: task.steps.into_iter().map(Into::into).collect(),
            steps_truncated: task.steps_truncated,
            active_children: task.active_children,
            completed_children: task.completed_children,
            failed_children: task.failed_children,
            lanes: task.lanes.into_iter().map(Into::into).collect(),
            lanes_truncated: task.lanes_truncated,
            can_continue: task.can_continue,
        }
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_control: Option<HttpConversationTaskControl>,
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
            task_control: page.task_control.map(Into::into),
        }
    }
}

/// Typed, bounded selector accepted by the authenticated display artifact endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum HttpToolArtifactSelector {
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

impl HttpToolArtifactSelector {
    /// Validates this transport selector against the kernel-owned retrieval policy.
    ///
    /// # Errors
    ///
    /// Returns an error when a byte, line, match, context, or literal bound is invalid.
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        let coordinate = match self {
            Self::ByteSlice { offset, .. } => *offset,
            Self::LinePage { start_line, .. } => *start_line,
            Self::SearchLiteral { start_offset, .. } => *start_offset,
        };
        if coordinate > TOOL_ARTIFACT_MAX_BYTES as u64 {
            anyhow::bail!("tool artifact selector coordinate exceeds the artifact hard limit");
        }
        ToolArtifactSelectorV1::from(self.clone()).validate()
    }
}

impl From<HttpToolArtifactSelector> for ToolArtifactSelectorV1 {
    fn from(selector: HttpToolArtifactSelector) -> Self {
        match selector {
            HttpToolArtifactSelector::ByteSlice { offset, limit } => {
                Self::ByteSlice { offset, limit }
            }
            HttpToolArtifactSelector::LinePage {
                start_line,
                line_count,
            } => Self::LinePage {
                start_line,
                line_count,
            },
            HttpToolArtifactSelector::SearchLiteral {
                query,
                start_offset,
                max_matches,
                context_lines,
            } => Self::SearchLiteral {
                query,
                start_offset,
                max_matches,
                context_lines,
            },
        }
    }
}

impl From<ToolArtifactSelectorV1> for HttpToolArtifactSelector {
    fn from(selector: ToolArtifactSelectorV1) -> Self {
        match selector {
            ToolArtifactSelectorV1::ByteSlice { offset, limit } => {
                Self::ByteSlice { offset, limit }
            }
            ToolArtifactSelectorV1::LinePage {
                start_line,
                line_count,
            } => Self::LinePage {
                start_line,
                line_count,
            },
            ToolArtifactSelectorV1::SearchLiteral {
                query,
                start_offset,
                max_matches,
                context_lines,
            } => Self::SearchLiteral {
                query,
                start_offset,
                max_matches,
                context_lines,
            },
        }
    }
}

/// Authenticated request for one display-safe artifact page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpToolArtifactReadRequest {
    /// Opaque session-scoped artifact reference. This is never a physical path.
    pub artifact_ref: String,
    pub selector: HttpToolArtifactSelector,
}

impl HttpToolArtifactReadRequest {
    /// Validates the opaque reference and selector without touching storage.
    ///
    /// # Errors
    ///
    /// Returns an error when the reference schema or selector bounds are invalid.
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        ToolArtifactRefV1 {
            artifact_id: self.artifact_ref.clone(),
        }
        .validate()?;
        self.selector.validate()
    }
}

/// Encoding of one bounded artifact page body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpToolArtifactPageEncoding {
    Utf8,
    Base64,
}

/// One bounded artifact page safe for authenticated local display clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpToolArtifactPage {
    pub schema_version: u16,
    /// Process-local adapter session id; the durable scope and physical path are omitted.
    pub request_scope: String,
    pub artifact_ref: String,
    pub selector: HttpToolArtifactSelector,
    pub body: String,
    pub body_encoding: HttpToolArtifactPageEncoding,
    pub returned_bytes: u32,
    pub page_sha256: String,
    pub artifact_sha256: String,
    pub eof: bool,
    pub match_count: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_selector: Option<HttpToolArtifactSelector>,
}

impl HttpToolArtifactPage {
    pub(crate) fn from_kernel(request_scope: &str, page: ToolArtifactPageV1) -> Self {
        Self {
            schema_version: HTTP_TOOL_ARTIFACT_PAGE_SCHEMA_VERSION,
            request_scope: request_scope.to_owned(),
            artifact_ref: page.artifact_ref.artifact_id,
            selector: page.selector.into(),
            body: page.body,
            body_encoding: match page.body_encoding {
                ToolArtifactPageEncoding::Utf8 => HttpToolArtifactPageEncoding::Utf8,
                ToolArtifactPageEncoding::Base64 => HttpToolArtifactPageEncoding::Base64,
            },
            returned_bytes: page.returned_bytes,
            page_sha256: page.page_sha256,
            artifact_sha256: page.artifact_sha256,
            eof: page.eof,
            match_count: page.match_count,
            next_selector: page.next_selector.map(Into::into),
        }
    }

    pub(crate) fn validate(
        &self,
        request_scope: &str,
        request: &HttpToolArtifactReadRequest,
    ) -> anyhow::Result<()> {
        if self.schema_version != HTTP_TOOL_ARTIFACT_PAGE_SCHEMA_VERSION
            || self.request_scope != request_scope
            || self.artifact_ref != request.artifact_ref
            || self.selector != request.selector
            || self.returned_bytes > sigil_kernel::session::TOOL_ARTIFACT_READ_MAX_BYTES
            || self.match_count > sigil_kernel::session::TOOL_ARTIFACT_SEARCH_MAX_MATCHES
            || match &self.selector {
                HttpToolArtifactSelector::ByteSlice { limit, .. } => self.returned_bytes > *limit,
                HttpToolArtifactSelector::SearchLiteral { max_matches, .. } => {
                    self.match_count > *max_matches
                }
                HttpToolArtifactSelector::LinePage { .. } => false,
            }
            || !valid_tool_artifact_hash(&self.page_sha256)
            || !valid_tool_artifact_hash(&self.artifact_sha256)
            || self
                .next_selector
                .as_ref()
                .is_some_and(|selector| selector.validate().is_err())
            || !valid_tool_artifact_continuation(
                &self.selector,
                self.next_selector.as_ref(),
                self.eof,
            )
            || (!matches!(
                &self.selector,
                HttpToolArtifactSelector::SearchLiteral { .. }
            ) && self.match_count != 0)
        {
            anyhow::bail!("tool artifact page violates the bounded response contract");
        }
        request.validate()?;
        let body_bytes = match self.body_encoding {
            HttpToolArtifactPageEncoding::Utf8 => self.body.as_bytes().to_vec(),
            HttpToolArtifactPageEncoding::Base64 => BASE64_STANDARD
                .decode(&self.body)
                .map_err(|_| anyhow::anyhow!("tool artifact body is not valid base64"))?,
        };
        if body_bytes.len() != self.returned_bytes as usize
            || body_bytes.len() > sigil_kernel::session::TOOL_ARTIFACT_READ_MAX_BYTES as usize
            || stable_event_hash(&body_bytes) != self.page_sha256
        {
            anyhow::bail!("tool artifact page body violates its integrity contract");
        }
        Ok(())
    }
}

fn valid_tool_artifact_hash(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn valid_tool_artifact_continuation(
    current: &HttpToolArtifactSelector,
    next: Option<&HttpToolArtifactSelector>,
    eof: bool,
) -> bool {
    if eof != next.is_none() {
        return false;
    }
    match (current, next) {
        (_, None) => true,
        (
            HttpToolArtifactSelector::ByteSlice { offset, limit },
            Some(HttpToolArtifactSelector::ByteSlice {
                offset: next_offset,
                limit: next_limit,
            }),
        ) => next_offset > offset && next_limit == limit,
        (
            HttpToolArtifactSelector::LinePage {
                start_line,
                line_count,
            },
            Some(HttpToolArtifactSelector::LinePage {
                start_line: next_line,
                line_count: next_count,
            }),
        ) => next_line > start_line && next_count == line_count,
        (
            HttpToolArtifactSelector::LinePage { .. },
            Some(HttpToolArtifactSelector::ByteSlice {
                offset: next_offset,
                ..
            }),
        ) => *next_offset > 0,
        (
            HttpToolArtifactSelector::SearchLiteral {
                query,
                start_offset,
                max_matches,
                context_lines,
            },
            Some(HttpToolArtifactSelector::SearchLiteral {
                query: next_query,
                start_offset: next_offset,
                max_matches: next_matches,
                context_lines: next_context,
            }),
        ) => {
            next_offset > start_offset
                && next_query == query
                && next_matches == max_matches
                && next_context == context_lines
        }
        _ => false,
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
                skill,
                assistant_phase,
                image_attachment_count,
                truncated,
                original_content_bytes,
            } => Self::Message {
                role: role.into(),
                text,
                skill: skill.map(|skill| HttpConversationDisplaySkillReference {
                    id: skill.id,
                    name: skill.name,
                }),
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
                artifact_ref,
                artifact_availability,
                observed_bytes,
                persisted_bytes,
                has_more,
            } => Self::Tool {
                call_id,
                tool_name,
                output,
                truncated,
                original_content_bytes: usize_as_u64(original_content_bytes),
                artifact_ref,
                artifact_availability,
                observed_bytes,
                persisted_bytes,
                has_more,
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
    WorkspaceMismatch, CurrentHashMismatch, IntentStateConflict, ArtifactUnavailable, SensitiveSnapshot,
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
    /// Exact durable Task continuation requested instead of a new conversation turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_continuation: Option<HttpTaskContinuationRequest>,
}

/// Exact durable Task continuation admitted through the foreground run control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpTaskContinuationRequest {
    /// Exact durable Task id selected from application projection truth.
    pub task_id: String,
    /// Optional user guidance applied at the Task continuation safe point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
}

/// Exact stale-safe Task pause request shared with TUI projection truth.
pub type HttpTaskPauseRequest = TaskPauseRequest;

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
    /// A different model requires creation of a fresh durable session.
    FreshSession,
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

/// Exact read-only target used to generate one current Intent Drop preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpIntentDropPreviewRequest {
    pub intent_ref: IntentVersionRef,
}

/// Renderer-safe durable Intent Stack projection.
pub type HttpIntentStackView = PublicIntentStackStateV1;

/// Exact digest-bound Drop preview shared with every application adapter.
pub type HttpIntentDropPreview = IntentOperationPreviewV1;

/// Renderer-to-runtime Drop request. It carries no file or approval authority.
pub type HttpIntentDropRequest = IntentDropRequestV1;

/// Terminal operation result derived by the application owner.
pub type HttpIntentDropExecution = IntentOperationExecutionV1;

/// Receipt for one idempotent exact Intent Drop command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpIntentDropCommandReceipt {
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub execution: HttpIntentDropExecution,
    pub replayed: bool,
}

impl HttpIntentDropCommandReceipt {
    pub(crate) fn replayed(mut self) -> Self {
        self.replayed = true;
        self
    }
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
    pub model_ref: HttpProviderModelRef,
    pub display_name: String,
    pub availability: String,
    pub recommendation: String,
    pub provenance: String,
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
    /// Compound connection/model identity durably frozen for this session.
    pub model_ref: HttpProviderModelRef,
    /// Provider identity durably frozen for this session.
    pub provider_name: String,
    /// Model identity durably frozen for this session.
    pub model_name: String,
    /// Exact connection-scoped catalog and effort capabilities for each selectable model.
    pub model_options: Vec<HttpApplicationModelOption>,
    /// Session boundary required to change the model.
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
    /// An exact durable Task pause has been requested and routed to the driver.
    PauseRequested,
    /// The driver boundary unwound and execution state requires durable reconciliation.
    ExecutionUncertain,
    /// The run has finished.
    Finished,
    /// The run failed or the driver rejected startup.
    Failed,
    /// Cooperative cancellation reached a durable clean terminal.
    Cancelled,
    /// The physical run quiesced and its exact durable Task is resumably paused.
    Paused,
    /// Execution stopped without proving a clean cancellation terminal.
    Interrupted,
}

impl HttpRunStatus {
    /// Returns whether the status is terminal for routing purposes.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Finished | Self::Failed | Self::Cancelled | Self::Paused | Self::Interrupted
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
    /// Cooperative cancellation reached durable quiescence for an exact Task pause.
    Paused,
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
            Self::Paused => HttpRunStatus::Paused,
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

/// Restore operation applied to one controlled checkpoint file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpCheckpointRestoreKind {
    RestoreContent,
    RemoveCreatedFile,
}

/// Whether durable evidence can restore one controlled checkpoint file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpCheckpointFileAvailability {
    Restorable,
    Sensitive,
    Unsupported,
    Unavailable,
}

/// Bounded renderer-safe file binding for one checkpoint row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpCheckpointFileView {
    pub path: String,
    pub restore_kind: HttpCheckpointRestoreKind,
    pub availability: HttpCheckpointFileAvailability,
}

/// Exact renderer-safe checkpoint binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpCheckpointView {
    pub checkpoint_id: String,
    pub checkpoint_digest: String,
    pub turn_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub files: Vec<HttpCheckpointFileView>,
    pub unknown_mutation_count: usize,
    pub fully_restorable: bool,
}

/// Exact finalized-turn binding available for a conversation-only fork.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationForkPointView {
    pub source_turn_index: usize,
    pub source_turn_digest: String,
    pub source_boundary_stream_sequence: u64,
    pub source_finalized_stream_sequence: u64,
}

/// Durable checkpoint/fork projection for one bound adapter session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationRecoveryView {
    pub checkpoints: Vec<HttpCheckpointView>,
    pub fork_points: Vec<HttpConversationForkPointView>,
    pub through_stream_sequence: u64,
}

/// Exact token economics shown before explicit portable compaction apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpCompactionEconomics {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_cost_nano_usd: Option<u64>,
}

/// Typed portable-compaction admission returned before activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum HttpCompactionAdmission {
    Prepared {
        standalone_tool_output_shrink_available: bool,
    },
    Ready {
        economics: HttpCompactionEconomics,
    },
    NoFoldableHistory {
        durable_message_count: usize,
        minimum_tail_turn_count: usize,
    },
    Unavailable {
        reason: String,
    },
}

/// Exact pre-activation compaction review. `preview_id` is process-local and required for apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpCompactionReview {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_id: Option<String>,
    pub folded_event_count: usize,
    pub retained_event_count: usize,
    pub policy: ApplicationCompactionPolicyView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Box<ApplicationCompactionDetailsView>>,
    pub admission: HttpCompactionAdmission,
}

impl From<ApplicationCompactionReview> for HttpCompactionReview {
    fn from(review: ApplicationCompactionReview) -> Self {
        Self {
            preview_id: review.preview_id,
            folded_event_count: review.folded_event_count,
            retained_event_count: review.retained_event_count,
            policy: review.policy,
            details: review.details,
            admission: match review.admission {
                ApplicationCompactionAdmission::Prepared {
                    standalone_tool_output_shrink_available,
                } => HttpCompactionAdmission::Prepared {
                    standalone_tool_output_shrink_available,
                },
                ApplicationCompactionAdmission::Ready { economics } => {
                    HttpCompactionAdmission::Ready {
                        economics: HttpCompactionEconomics {
                            before_input_tokens: economics.before_input_tokens,
                            target_input_tokens: economics.target_input_tokens,
                            context_window_tokens: economics.context_window_tokens,
                            output_tokens: economics.output_tokens,
                            safety_buffer_tokens: economics.safety_buffer_tokens,
                            savings_tokens: economics.savings_tokens,
                            savings_ratio_ppm: economics.savings_ratio_ppm,
                            minimum_savings_tokens: economics.minimum_savings_tokens,
                            minimum_savings_ratio_ppm: economics.minimum_savings_ratio_ppm,
                            summary_cache_read_tokens: economics.summary_cache_read_tokens,
                            summary_uncached_input_tokens: economics.summary_uncached_input_tokens,
                            summary_output_tokens: economics.summary_output_tokens,
                            summary_cost_nano_usd: economics.summary_cost_nano_usd,
                        },
                    }
                }
                ApplicationCompactionAdmission::NoFoldableHistory {
                    durable_message_count,
                    minimum_tail_turn_count,
                } => HttpCompactionAdmission::NoFoldableHistory {
                    durable_message_count,
                    minimum_tail_turn_count,
                },
                ApplicationCompactionAdmission::Unavailable { reason } => {
                    HttpCompactionAdmission::Unavailable { reason }
                }
            },
        }
    }
}

impl From<ApplicationConversationRecoveryView> for HttpConversationRecoveryView {
    fn from(view: ApplicationConversationRecoveryView) -> Self {
        Self {
            checkpoints: view
                .checkpoints
                .into_iter()
                .map(|checkpoint| HttpCheckpointView {
                    checkpoint_id: checkpoint.checkpoint_id,
                    checkpoint_digest: checkpoint.checkpoint_digest,
                    turn_index: checkpoint.turn_index,
                    prompt: checkpoint.prompt,
                    files: checkpoint
                        .files
                        .into_iter()
                        .map(|file| HttpCheckpointFileView {
                            path: file.path.to_string_lossy().into_owned(),
                            restore_kind: match file.restore_kind {
                                sigil_kernel::ControlledCheckpointRestoreKind::RestoreContent => {
                                    HttpCheckpointRestoreKind::RestoreContent
                                }
                                sigil_kernel::ControlledCheckpointRestoreKind::RemoveCreatedFile => {
                                    HttpCheckpointRestoreKind::RemoveCreatedFile
                                }
                            },
                            availability: match file.availability {
                                sigil_kernel::ControlledCheckpointFileAvailability::Restorable => {
                                    HttpCheckpointFileAvailability::Restorable
                                }
                                sigil_kernel::ControlledCheckpointFileAvailability::Sensitive => {
                                    HttpCheckpointFileAvailability::Sensitive
                                }
                                sigil_kernel::ControlledCheckpointFileAvailability::Unsupported => {
                                    HttpCheckpointFileAvailability::Unsupported
                                }
                                sigil_kernel::ControlledCheckpointFileAvailability::Unavailable => {
                                    HttpCheckpointFileAvailability::Unavailable
                                }
                            },
                        })
                        .collect(),
                    unknown_mutation_count: checkpoint.unknown_mutation_count,
                    fully_restorable: checkpoint.fully_restorable,
                })
                .collect(),
            fork_points: view
                .fork_points
                .into_iter()
                .map(|point| HttpConversationForkPointView {
                    source_turn_index: point.source_turn_index,
                    source_turn_digest: point.source_turn_digest,
                    source_boundary_stream_sequence: point.source_boundary_stream_sequence,
                    source_finalized_stream_sequence: point.source_finalized_stream_sequence,
                })
                .collect(),
            through_stream_sequence: view.through_stream_sequence,
        }
    }
}

/// Exact checkpoint binding submitted for read-only restore preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpCheckpointRestoreRequest {
    pub checkpoint_id: String,
    pub checkpoint_digest: String,
}

impl From<&HttpCheckpointRestoreRequest> for sigil_kernel::ControlledCheckpointRestoreRequest {
    fn from(request: &HttpCheckpointRestoreRequest) -> Self {
        Self {
            checkpoint_id: request.checkpoint_id.clone(),
            checkpoint_digest: request.checkpoint_digest.clone(),
        }
    }
}

/// File-level conflict direction returned by a fresh restore preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpCheckpointRestoreConflictReason {
    WorkspaceMismatch,
    CurrentHashMismatch,
    IntentStateConflict,
    ArtifactUnavailable,
    SensitiveSnapshot,
    UnsupportedSnapshot,
    InvalidBinding,
}

/// Fresh file preflight for one checkpoint restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpCheckpointRestorePreviewFile {
    pub path: String,
    pub restore_kind: HttpCheckpointRestoreKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_current_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_current_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_reason: Option<HttpCheckpointRestoreConflictReason>,
}

/// Bounded reverse diff captured for one controlled checkpoint file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpCheckpointReverseDiff {
    pub path: String,
    pub diff: String,
    pub truncated: bool,
    pub original_line_count: usize,
}

/// Exact restore review returned without mutating durable or workspace truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpCheckpointRestoreReview {
    pub checkpoint_id: String,
    pub checkpoint_digest: String,
    pub files: Vec<HttpCheckpointRestorePreviewFile>,
    pub reverse_diffs: Vec<HttpCheckpointReverseDiff>,
    pub unknown_mutation_count: usize,
    pub ready: bool,
}

impl From<ApplicationCheckpointRestoreReview> for HttpCheckpointRestoreReview {
    fn from(review: ApplicationCheckpointRestoreReview) -> Self {
        let preview = review.preview;
        Self {
            checkpoint_id: preview.checkpoint_id,
            checkpoint_digest: preview.checkpoint_digest,
            files: preview
                .files
                .into_iter()
                .map(|file| HttpCheckpointRestorePreviewFile {
                    path: file.path.to_string_lossy().into_owned(),
                    restore_kind: match file.restore_kind {
                        sigil_kernel::ControlledCheckpointRestoreKind::RestoreContent => {
                            HttpCheckpointRestoreKind::RestoreContent
                        }
                        sigil_kernel::ControlledCheckpointRestoreKind::RemoveCreatedFile => {
                            HttpCheckpointRestoreKind::RemoveCreatedFile
                        }
                    },
                    expected_current_hash: file.expected_current_hash,
                    actual_current_hash: file.actual_current_hash,
                    conflict_reason: file.conflict_reason.map(|reason| match reason {
                        sigil_kernel::CheckpointRestoreConflictReason::WorkspaceMismatch => {
                            HttpCheckpointRestoreConflictReason::WorkspaceMismatch
                        }
                        sigil_kernel::CheckpointRestoreConflictReason::CurrentHashMismatch => {
                            HttpCheckpointRestoreConflictReason::CurrentHashMismatch
                        }
                        sigil_kernel::CheckpointRestoreConflictReason::IntentStateConflict => {
                            HttpCheckpointRestoreConflictReason::IntentStateConflict
                        }
                        sigil_kernel::CheckpointRestoreConflictReason::ArtifactUnavailable => {
                            HttpCheckpointRestoreConflictReason::ArtifactUnavailable
                        }
                        sigil_kernel::CheckpointRestoreConflictReason::SensitiveSnapshot => {
                            HttpCheckpointRestoreConflictReason::SensitiveSnapshot
                        }
                        sigil_kernel::CheckpointRestoreConflictReason::UnsupportedSnapshot => {
                            HttpCheckpointRestoreConflictReason::UnsupportedSnapshot
                        }
                        sigil_kernel::CheckpointRestoreConflictReason::InvalidBinding => {
                            HttpCheckpointRestoreConflictReason::InvalidBinding
                        }
                    }),
                })
                .collect(),
            reverse_diffs: review
                .reverse_diffs
                .into_iter()
                .map(|diff| HttpCheckpointReverseDiff {
                    path: diff.path.to_string_lossy().into_owned(),
                    diff: diff.diff,
                    truncated: diff.truncated,
                    original_line_count: diff.original_line_count,
                })
                .collect(),
            unknown_mutation_count: preview.unknown_mutation_count,
            ready: preview.ready,
        }
    }
}

/// Exact recovery mutation requested under an envelope command id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum HttpConversationRecoveryCommandAction {
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
        model_ref: HttpProviderModelRef,
    },
}

/// Stable action kind echoed by a recovery command receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpConversationRecoveryCommandActionKind {
    PrepareCompaction,
    ApplyCompaction,
    ApplyStandaloneToolOutputShrink,
    RestoreCheckpoint,
    ForkConversation,
}

impl HttpConversationRecoveryCommandAction {
    #[must_use]
    pub fn kind(&self) -> HttpConversationRecoveryCommandActionKind {
        match self {
            Self::PrepareCompaction { .. } => {
                HttpConversationRecoveryCommandActionKind::PrepareCompaction
            }
            Self::ApplyCompaction { .. } => {
                HttpConversationRecoveryCommandActionKind::ApplyCompaction
            }
            Self::ApplyStandaloneToolOutputShrink { .. } => {
                HttpConversationRecoveryCommandActionKind::ApplyStandaloneToolOutputShrink
            }
            Self::RestoreCheckpoint { .. } => {
                HttpConversationRecoveryCommandActionKind::RestoreCheckpoint
            }
            Self::ForkConversation { .. } => {
                HttpConversationRecoveryCommandActionKind::ForkConversation
            }
        }
    }
}

/// Durable portable-compaction receipt fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpCompactionReceipt {
    pub compaction_id: String,
    pub attempt_id: String,
    pub task_memory_id: String,
    pub folded_event_count: usize,
    pub tool_output_projection_recorded: bool,
    pub native_carrier_materialized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_carrier_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpToolOutputShrinkReceipt {
    pub context_epoch_id: String,
    pub projected_output_count: usize,
}

/// Durable restore-specific receipt fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpCheckpointRestoreReceipt {
    pub checkpoint_id: String,
    pub batch_id: String,
    pub restored_file_count: usize,
    pub verification_stale: bool,
}

/// Durable fork-specific receipt fields. The returned reference must be reopened explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationForkReceipt {
    pub session_ref: String,
    pub session_id: String,
    pub copied_message_count: usize,
    pub copied_external_provenance_count: usize,
}

/// Durable idempotent receipt for one restore or conversation-fork command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpConversationRecoveryCommandReceipt {
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    pub action: HttpConversationRecoveryCommandActionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<HttpCompactionReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_review: Option<HttpCompactionReview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_output_shrink: Option<HttpToolOutputShrinkReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore: Option<HttpCheckpointRestoreReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<HttpConversationForkReceipt>,
    pub recovery: HttpConversationRecoveryView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub replayed: bool,
}

impl HttpConversationRecoveryCommandReceipt {
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

/// Receipt for an envelope-routed exact Task pause command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpTaskPauseCommandReceipt {
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
    /// Exact Task selected by the rendered pause action.
    pub task_id: String,
    /// Accepted plan incarnation selected by the rendered pause action.
    pub plan_version: u32,
    /// Run snapshot after the pause reached a durable terminal.
    pub run: HttpRunSnapshot,
    /// Whether this response was replayed from a prior command id.
    pub replayed: bool,
}

impl HttpTaskPauseCommandReceipt {
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

/// Exact stale-safe integration review identity shared with durable Task projection truth.
pub type HttpTaskIntegrationReviewRequest = TaskIntegrationReviewRequest;

/// Renderer-safe final promotion target kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpIntegrationPromotionTargetKind {
    WorkspaceApply,
    GitRefAdvance,
}

impl From<ApplicationIntegrationPromotionTargetKind> for HttpIntegrationPromotionTargetKind {
    fn from(value: ApplicationIntegrationPromotionTargetKind) -> Self {
        match value {
            ApplicationIntegrationPromotionTargetKind::WorkspaceApply => Self::WorkspaceApply,
            ApplicationIntegrationPromotionTargetKind::GitRefAdvance => Self::GitRefAdvance,
        }
    }
}

/// Renderer-safe physical integration lane candidate kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpIntegrationLaneCandidateKind {
    ManagedRef,
    SnapshotWorkspace,
}

impl From<ApplicationIntegrationLaneCandidateKind> for HttpIntegrationLaneCandidateKind {
    fn from(value: ApplicationIntegrationLaneCandidateKind) -> Self {
        match value {
            ApplicationIntegrationLaneCandidateKind::ManagedRef => Self::ManagedRef,
            ApplicationIntegrationLaneCandidateKind::SnapshotWorkspace => Self::SnapshotWorkspace,
        }
    }
}

/// Bounded, private-ref-free provenance for one reviewed integration lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpTaskIntegrationLaneView {
    pub lane_id: String,
    pub candidate_kind: HttpIntegrationLaneCandidateKind,
    pub proposal_count: usize,
    pub verification_receipt_count: usize,
}

/// Exact current integration review returned to an authenticated application client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpTaskIntegrationReviewView {
    pub schema_version: u16,
    pub request: HttpTaskIntegrationReviewRequest,
    pub aggregate_diff: String,
    pub aggregate_diff_digest: String,
    pub preview_digest: String,
    pub policy_digest: String,
    pub target_kind: HttpIntegrationPromotionTargetKind,
    pub lanes: Vec<HttpTaskIntegrationLaneView>,
    pub child_verification_receipt_count: usize,
    pub lane_verification_receipt_count: usize,
    pub conflict_reasons: Vec<String>,
    pub verification_invalidation_count: usize,
    pub parent_verification_pending: bool,
}

impl From<ApplicationTaskIntegrationReviewView> for HttpTaskIntegrationReviewView {
    fn from(value: ApplicationTaskIntegrationReviewView) -> Self {
        Self {
            schema_version: value.schema_version,
            request: value.request,
            aggregate_diff: value.aggregate_diff,
            aggregate_diff_digest: value.aggregate_diff_digest,
            preview_digest: value.preview_digest,
            policy_digest: value.policy_digest,
            target_kind: value.target_kind.into(),
            lanes: value
                .lanes
                .into_iter()
                .map(|lane| HttpTaskIntegrationLaneView {
                    lane_id: lane.lane_id,
                    candidate_kind: lane.candidate_kind.into(),
                    proposal_count: lane.proposal_count,
                    verification_receipt_count: lane.verification_receipt_count,
                })
                .collect(),
            child_verification_receipt_count: value.child_verification_receipt_count,
            lane_verification_receipt_count: value.lane_verification_receipt_count,
            conflict_reasons: value.conflict_reasons,
            verification_invalidation_count: value.verification_invalidation_count,
            parent_verification_pending: value.parent_verification_pending,
        }
    }
}

/// Terminal, renderer-safe result of accepting one exact integration review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpTaskIntegrationAcceptanceView {
    pub request: HttpTaskIntegrationReviewRequest,
    pub promotion_status: sigil_kernel::IntegrationPromotionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_verdict: Option<sigil_kernel::VerificationVerdict>,
    pub can_continue: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion_cleanup_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_cleanup_error: Option<String>,
}

impl From<ApplicationTaskIntegrationAcceptanceView> for HttpTaskIntegrationAcceptanceView {
    fn from(value: ApplicationTaskIntegrationAcceptanceView) -> Self {
        Self {
            request: value.request,
            promotion_status: value.promotion_status,
            parent_verdict: value.parent_verdict,
            can_continue: value.can_continue,
            promotion_cleanup_error: value.promotion_cleanup_error,
            parent_cleanup_error: value.parent_cleanup_error,
        }
    }
}

/// Receipt for an idempotent exact integration acceptance command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpTaskIntegrationAcceptanceCommandReceipt {
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub acceptance: HttpTaskIntegrationAcceptanceView,
    pub replayed: bool,
}

impl HttpTaskIntegrationAcceptanceCommandReceipt {
    pub(crate) fn replayed(mut self) -> Self {
        self.replayed = true;
        self
    }
}
