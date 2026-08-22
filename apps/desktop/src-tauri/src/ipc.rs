use serde::{Deserialize, Serialize};

mod plan_input;
pub(crate) use plan_input::*;
use sigil_desktop::{
    DesktopAgentActivityStatus, DesktopAgentActivityView, DesktopAgentHandoffStatus,
    DesktopApplicationClientAction, DesktopApprovalCommandReceipt as NativeApprovalCommandReceipt,
    DesktopCheckpointRestoreReview as NativeCheckpointRestoreReview,
    DesktopCompactionAdmission as NativeCompactionAdmission,
    DesktopCompactionReview as NativeCompactionReview, DesktopContextWindowSource,
    DesktopConversationDisplayApprovalDecision as NativeConversationDisplayApprovalDecision,
    DesktopConversationDisplayAssistantPhase as NativeConversationDisplayAssistantPhase,
    DesktopConversationDisplayCheckpointConflictReason as NativeConversationDisplayCheckpointConflictReason,
    DesktopConversationDisplayCheckpointOutcome as NativeConversationDisplayCheckpointOutcome,
    DesktopConversationDisplayContent as NativeConversationDisplayContent,
    DesktopConversationDisplayItem as NativeConversationDisplayItem,
    DesktopConversationDisplayItemKind as NativeConversationDisplayItemKind,
    DesktopConversationDisplayMessageRole as NativeConversationDisplayMessageRole,
    DesktopConversationDisplayPage as NativeConversationDisplayPage,
    DesktopConversationDisplaySource as NativeConversationDisplaySource,
    DesktopConversationDisplayStatus as NativeConversationDisplayStatus,
    DesktopConversationQueueCommandAction as NativeConversationQueueCommandAction,
    DesktopConversationQueueCommandActionKind as NativeConversationQueueCommandActionKind,
    DesktopConversationQueueCommandReceipt as NativeConversationQueueCommandReceipt,
    DesktopConversationQueueItem as NativeConversationQueueItem,
    DesktopConversationQueueItemKind as NativeConversationQueueItemKind,
    DesktopConversationQueueView as NativeConversationQueueView,
    DesktopConversationRecoveryCommandAction as NativeConversationRecoveryCommandAction,
    DesktopConversationRecoveryCommandActionKind as NativeConversationRecoveryCommandActionKind,
    DesktopConversationRecoveryCommandReceipt as NativeConversationRecoveryCommandReceipt,
    DesktopConversationRecoveryView as NativeConversationRecoveryView,
    DesktopConversationTaskControl as NativeConversationTaskControl,
    DesktopConversationTaskLane as NativeConversationTaskLane,
    DesktopConversationTaskPlanStep as NativeConversationTaskPlanStep,
    DesktopIntegrationLaneCandidateKind, DesktopIntegrationPromotionStatus,
    DesktopIntegrationPromotionTargetKind, DesktopModelSelectionPolicy, DesktopPermissionMode,
    DesktopProviderConnectionInventory, DesktopProviderConnectionReadiness,
    DesktopProviderCredentialSource, DesktopProviderSetupCatalog,
    DesktopProviderSetupCatalogRequest, DesktopProviderSetupCredentialSource,
    DesktopProviderSetupProtocol, DesktopProviderSetupSaveRequest, DesktopProviderSetupSaveResult,
    DesktopProviderSetupTemplate, DesktopPublicTaskPhase, DesktopReasoningEffort,
    DesktopRunContextView, DesktopRunSnapshot, DesktopRunStatus, DesktopSessionCatalogBatchAction,
    DesktopSessionCatalogBatchOutcome, DesktopSessionCatalogBatchPlan,
    DesktopSessionCatalogBatchPlanStatus, DesktopSessionCatalogBatchReceipt,
    DesktopSessionCatalogEntry, DesktopSessionCatalogPage, DesktopSessionCatalogSourceDiagnostic,
    DesktopSessionCatalogState, DesktopSessionRouteRecoveryAction, DesktopSessionRouteRecoveryCode,
    DesktopSessionRouteRecoveryView, DesktopSessionRouteTransitionKind, DesktopSessionSnapshot,
    DesktopSessionTranscriptMessage, DesktopSessionTranscriptPage, DesktopSupportCheck,
    DesktopSupportDoctorReport, DesktopSupportEnvironment, DesktopSupportPrivacy,
    DesktopSupportStatus, DesktopSupportSummary, DesktopTaskIntegrationAcceptanceView,
    DesktopTaskIntegrationReviewRequest, DesktopTaskIntegrationReviewView, DesktopTimelineEvent,
    DesktopTimelineTaskChecklistItem, DesktopTimelineTaskExecutionBinding,
    DesktopTimelineTerminalTask, DesktopToolArtifactAvailability as NativeToolArtifactAvailability,
    DesktopToolArtifactPage as NativeToolArtifactPage,
    DesktopToolArtifactPageEncoding as NativeToolArtifactPageEncoding,
    DesktopToolArtifactSelector as NativeToolArtifactSelector, DesktopTranscriptAssistantKind,
    DesktopTranscriptRole, DesktopVerificationAction, DesktopVerificationCheckStatus,
    DesktopVerificationRerunRequest, DesktopVerificationScope, DesktopVerificationVerdict,
    DesktopVerificationView, DesktopWorkspaceSummary,
};

use crate::{
    appearance::{AppearanceSnapshot, ThemePreference},
    recent::RecentWorkspaceSummary,
    run_streams::DesktopRunStreamState,
};

mod intent_stack;
pub(crate) use intent_stack::*;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopBootstrap {
    pub(crate) protocol_version: u16,
    pub(crate) workspaces: Vec<DesktopWorkspaceSummary>,
    pub(crate) recent_workspaces: Vec<RecentWorkspaceSummary>,
    pub(crate) appearance: AppearanceSnapshot,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSupportDoctorSummary {
    generated_at_unix_ms: u64,
    version: String,
    commit: String,
    target: String,
    profile: String,
    environment: DesktopSupportEnvironmentSummary,
    summary: DesktopSupportStatusSummary,
    checks: Vec<DesktopSupportCheckSummary>,
    privacy: DesktopSupportPrivacySummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopSupportEnvironmentSummary {
    os: String,
    architecture: String,
    terminal_family: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopSupportStatusSummary {
    overall_status: &'static str,
    ok: usize,
    warn: usize,
    error: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopSupportCheckSummary {
    status: &'static str,
    name: String,
    summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    remediation: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopSupportPrivacySummary {
    included: Vec<String>,
    excluded: Vec<String>,
    review_before_sharing: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSupportSaveSummary {
    pub(crate) cancelled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file_name: Option<String>,
}

impl From<DesktopSupportDoctorReport> for DesktopSupportDoctorSummary {
    fn from(value: DesktopSupportDoctorReport) -> Self {
        Self {
            generated_at_unix_ms: value.generated_at_unix_ms,
            version: value.version,
            commit: value.commit,
            target: value.target,
            profile: value.profile,
            environment: value.environment.into(),
            summary: value.summary.into(),
            checks: value.checks.into_iter().map(Into::into).collect(),
            privacy: value.privacy.into(),
        }
    }
}

impl From<DesktopSupportEnvironment> for DesktopSupportEnvironmentSummary {
    fn from(value: DesktopSupportEnvironment) -> Self {
        Self {
            os: value.os,
            architecture: value.architecture,
            terminal_family: value.terminal_family,
        }
    }
}

impl From<DesktopSupportSummary> for DesktopSupportStatusSummary {
    fn from(value: DesktopSupportSummary) -> Self {
        Self {
            overall_status: support_status_label(value.overall_status),
            ok: value.ok,
            warn: value.warn,
            error: value.error,
        }
    }
}

impl From<DesktopSupportCheck> for DesktopSupportCheckSummary {
    fn from(value: DesktopSupportCheck) -> Self {
        Self {
            status: support_status_label(value.status),
            name: value.name,
            summary: value.summary,
            remediation: value.remediation,
        }
    }
}

impl From<DesktopSupportPrivacy> for DesktopSupportPrivacySummary {
    fn from(value: DesktopSupportPrivacy) -> Self {
        Self {
            included: value.included,
            excluded: value.excluded,
            review_before_sharing: value.review_before_sharing,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopProviderSetupTemplateInput {
    DeepSeek,
    OpenAi,
    Anthropic,
    Gemini,
    OpenAiCompatible,
}

impl From<DesktopProviderSetupTemplateInput> for DesktopProviderSetupTemplate {
    fn from(value: DesktopProviderSetupTemplateInput) -> Self {
        match value {
            DesktopProviderSetupTemplateInput::DeepSeek => Self::DeepSeek,
            DesktopProviderSetupTemplateInput::OpenAi => Self::OpenAi,
            DesktopProviderSetupTemplateInput::Anthropic => Self::Anthropic,
            DesktopProviderSetupTemplateInput::Gemini => Self::Gemini,
            DesktopProviderSetupTemplateInput::OpenAiCompatible => Self::OpenAiCompatible,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopProviderSetupCredentialSourceInput {
    Environment,
    SecureStore,
    None,
}

impl From<DesktopProviderSetupCredentialSourceInput> for DesktopProviderSetupCredentialSource {
    fn from(value: DesktopProviderSetupCredentialSourceInput) -> Self {
        match value {
            DesktopProviderSetupCredentialSourceInput::Environment => Self::Environment,
            DesktopProviderSetupCredentialSourceInput::SecureStore => Self::SecureStore,
            DesktopProviderSetupCredentialSourceInput::None => Self::None,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopProviderSetupProtocolInput {
    Responses,
    ChatCompletions,
}

impl From<DesktopProviderSetupProtocolInput> for DesktopProviderSetupProtocol {
    fn from(value: DesktopProviderSetupProtocolInput) -> Self {
        match value {
            DesktopProviderSetupProtocolInput::Responses => Self::Responses,
            DesktopProviderSetupProtocolInput::ChatCompletions => Self::ChatCompletions,
        }
    }
}

/// Secret-bearing renderer input. It intentionally has no `Debug`, `Clone`, or serialization.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopProviderSetupCatalogInput {
    template: DesktopProviderSetupTemplateInput,
    protocol: Option<DesktopProviderSetupProtocolInput>,
    endpoint: Option<String>,
    credential_source: DesktopProviderSetupCredentialSourceInput,
    api_key: Option<String>,
    #[serde(default)]
    replace_invalid_config: bool,
}

impl DesktopProviderSetupCatalogInput {
    pub(crate) fn into_native(self) -> DesktopProviderSetupCatalogRequest {
        DesktopProviderSetupCatalogRequest {
            template: self.template.into(),
            protocol: self.protocol.map(Into::into),
            endpoint: self.endpoint,
            credential_source: self.credential_source.into(),
            api_key: self.api_key,
            replace_invalid_config: self.replace_invalid_config,
        }
    }
}

/// Secret-bearing renderer input. It intentionally has no `Debug`, `Clone`, or serialization.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopProviderSetupSaveInput {
    template: DesktopProviderSetupTemplateInput,
    protocol: Option<DesktopProviderSetupProtocolInput>,
    endpoint: Option<String>,
    credential_source: DesktopProviderSetupCredentialSourceInput,
    api_key: Option<String>,
    model_id: String,
    context_window_tokens: Option<u32>,
    label: Option<String>,
    #[serde(default)]
    replace_invalid_config: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopProviderDefaultModelSaveInput {
    model_ref: DesktopProviderModelRefInput,
    context_window_tokens: Option<u32>,
}

impl DesktopProviderDefaultModelSaveInput {
    pub(crate) fn into_native(self) -> sigil_desktop::DesktopProviderDefaultModelSaveRequest {
        sigil_desktop::DesktopProviderDefaultModelSaveRequest {
            model_ref: sigil_desktop::DesktopProviderModelRef {
                connection_id: self.model_ref.connection_id,
                model_id: self.model_ref.model_id,
            },
            context_window_tokens: self.context_window_tokens,
        }
    }
}

impl DesktopProviderSetupSaveInput {
    pub(crate) fn into_native(self) -> DesktopProviderSetupSaveRequest {
        DesktopProviderSetupSaveRequest {
            template: self.template.into(),
            protocol: self.protocol.map(Into::into),
            endpoint: self.endpoint,
            credential_source: self.credential_source.into(),
            api_key: self.api_key,
            model_id: self.model_id,
            context_window_tokens: self.context_window_tokens,
            label: self.label,
            replace_invalid_config: self.replace_invalid_config,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopProviderConnectionInventorySummary {
    config_mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_model: Option<DesktopProviderModelRefSummary>,
    connections: Vec<DesktopProviderConnectionSummary>,
    issues: Vec<DesktopProviderConnectionIssueSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopProviderConnectionSummary {
    id: String,
    label: String,
    provider_label: String,
    protocol_label: String,
    endpoint_display: String,
    credential_source: &'static str,
    readiness: &'static str,
    model_context_windows: std::collections::BTreeMap<String, u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_model: Option<DesktopProviderModelRefSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue: Option<DesktopProviderConnectionIssueSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopProviderConnectionIssueSummary {
    code: String,
    message: String,
}

impl From<DesktopProviderConnectionInventory> for DesktopProviderConnectionInventorySummary {
    fn from(value: DesktopProviderConnectionInventory) -> Self {
        Self {
            config_mode: match value.config_mode {
                sigil_desktop::DesktopProviderConfigMode::V2 => "v2",
                sigil_desktop::DesktopProviderConfigMode::Invalid => "invalid",
            },
            default_model: value.default_model.map(Into::into),
            connections: value
                .connections
                .into_iter()
                .map(|connection| DesktopProviderConnectionSummary {
                    id: connection.id,
                    label: connection.label,
                    provider_label: connection.provider_label,
                    protocol_label: connection.protocol_label,
                    endpoint_display: connection.endpoint_display,
                    credential_source: provider_credential_source_label(
                        connection.credential_source,
                    ),
                    readiness: provider_readiness_label(connection.readiness),
                    model_context_windows: connection.model_context_windows,
                    default_model: connection.default_model.map(Into::into),
                    issue: connection
                        .issue
                        .map(|issue| DesktopProviderConnectionIssueSummary {
                            code: issue.code,
                            message: issue.message,
                        }),
                })
                .collect(),
            issues: value
                .issues
                .into_iter()
                .map(|issue| DesktopProviderConnectionIssueSummary {
                    code: issue.code,
                    message: issue.message,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopProviderSetupCatalogSummary {
    connection_id: String,
    provider_label: String,
    state: String,
    models: Vec<DesktopProviderSetupModelSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggested_model: Option<String>,
    manual_entry_allowed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopProviderSetupModelSummary {
    model_id: String,
    display_name: String,
    availability: String,
    recommended: bool,
    provenance: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_window_tokens: Option<u32>,
}

impl From<DesktopProviderSetupCatalog> for DesktopProviderSetupCatalogSummary {
    fn from(value: DesktopProviderSetupCatalog) -> Self {
        Self {
            connection_id: value.connection_id,
            provider_label: value.provider_label,
            state: value.state,
            models: value
                .models
                .into_iter()
                .map(|model| DesktopProviderSetupModelSummary {
                    model_id: model.model_id,
                    display_name: model.display_name,
                    availability: model.availability,
                    recommended: model.recommended,
                    provenance: model.provenance,
                    context_window_tokens: model.context_window_tokens,
                })
                .collect(),
            suggested_model: value.suggested_model,
            manual_entry_allowed: value.manual_entry_allowed,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopProviderSetupSaveSummary {
    default_model: DesktopProviderModelRefSummary,
    inventory: DesktopProviderConnectionInventorySummary,
    save_warning: bool,
}

impl From<DesktopProviderSetupSaveResult> for DesktopProviderSetupSaveSummary {
    fn from(value: DesktopProviderSetupSaveResult) -> Self {
        Self {
            default_model: value.default_model.into(),
            inventory: value.inventory.into(),
            save_warning: value.save_warning,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopProviderDefaultModelSaveSummary {
    default_model: DesktopProviderModelRefSummary,
    inventory: DesktopProviderConnectionInventorySummary,
    save_warning: bool,
}

impl From<sigil_desktop::DesktopProviderDefaultModelSaveResult>
    for DesktopProviderDefaultModelSaveSummary
{
    fn from(value: sigil_desktop::DesktopProviderDefaultModelSaveResult) -> Self {
        Self {
            default_model: value.default_model.into(),
            inventory: value.inventory.into(),
            save_warning: value.save_warning,
        }
    }
}

fn provider_credential_source_label(value: DesktopProviderCredentialSource) -> &'static str {
    match value {
        DesktopProviderCredentialSource::Environment => "environment",
        DesktopProviderCredentialSource::Stored => "stored",
        DesktopProviderCredentialSource::None => "none",
    }
}

fn provider_readiness_label(value: DesktopProviderConnectionReadiness) -> &'static str {
    match value {
        DesktopProviderConnectionReadiness::Ready => "ready",
        DesktopProviderConnectionReadiness::NeedsCredential => "needs_credential",
        DesktopProviderConnectionReadiness::CredentialUnavailable => "credential_unavailable",
        DesktopProviderConnectionReadiness::NeedsModel => "needs_model",
        DesktopProviderConnectionReadiness::Unverified => "unverified",
        DesktopProviderConnectionReadiness::Invalid => "invalid",
    }
}

fn support_status_label(value: DesktopSupportStatus) -> &'static str {
    match value {
        DesktopSupportStatus::Ok => "ok",
        DesktopSupportStatus::Warn => "warn",
        DesktopSupportStatus::Error => "error",
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopAppearanceInput {
    pub(crate) preference: ThemePreference,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopExternalUrlInput {
    pub(crate) url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopWorkspaceSelection {
    pub(crate) cancelled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) workspace: Option<DesktopWorkspaceSummary>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopCatalogRequest {
    pub(crate) limit: Option<u16>,
    pub(crate) cursor: Option<String>,
    pub(crate) query: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) pinned: Option<bool>,
    pub(crate) state: Option<DesktopCatalogState>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopCatalogState {
    Ready,
    Oversized,
    ScanBudgetExceeded,
    Invalid,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum DesktopCatalogSourceDiagnostic {
    UnsafeSource,
    InvalidEventStream,
    InvalidProjection,
    MissingSessionIdentity,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCatalogPage {
    workspace_id: String,
    generation: u64,
    reconciled_at_unix_ms: u64,
    degraded_source_count: u64,
    identity_conflict_count: u64,
    truncated_source_count: u64,
    entries: Vec<DesktopCatalogEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopCatalogEntry {
    session_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    source_state: DesktopCatalogState,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_diagnostic: Option<DesktopCatalogSourceDiagnostic>,
    source_bytes: u64,
    source_modified_at_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    user_message_count: u64,
    assistant_message_count: u64,
    tool_result_count: u64,
    pinned: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopSessionCreateInput {
    pub(crate) label: Option<String>,
    pub(crate) model_ref: Option<DesktopProviderModelRefInput>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopProviderModelRefInput {
    pub(crate) connection_id: String,
    pub(crate) model_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopProviderModelRefSummary {
    pub(crate) connection_id: String,
    pub(crate) model_id: String,
}

impl From<sigil_desktop::DesktopProviderModelRef> for DesktopProviderModelRefSummary {
    fn from(value: sigil_desktop::DesktopProviderModelRef) -> Self {
        Self {
            connection_id: value.connection_id,
            model_id: value.model_id,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopSessionOpenInput {
    pub(crate) session_ref: String,
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) label: Option<String>,
    #[serde(default)]
    pub(crate) recovery_binding: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopSessionRenameInput {
    pub(crate) session_ref: String,
    pub(crate) session_id: String,
    pub(crate) display_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopSessionDeleteInput {
    pub(crate) session_ref: String,
    pub(crate) session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopSessionQuarantineInput {
    pub(crate) session_ref: String,
    pub(crate) source_bytes: u64,
    pub(crate) source_modified_at_unix_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopSessionInvalidSourceDeleteInput {
    pub(crate) session_ref: String,
    pub(crate) source_bytes: u64,
    pub(crate) source_modified_at_unix_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopSessionCatalogBatchItemInput {
    pub(crate) session_ref: String,
    #[serde(default)]
    pub(crate) session_id: Option<String>,
    #[serde(default)]
    pub(crate) source_bytes: Option<u64>,
    #[serde(default)]
    pub(crate) source_modified_at_unix_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopSessionCatalogBatchPlanInput {
    pub(crate) action: DesktopSessionCatalogBatchAction,
    pub(crate) items: Vec<DesktopSessionCatalogBatchItemInput>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopSessionCatalogBatchExecuteInput {
    pub(crate) plan_id: String,
    pub(crate) action: DesktopSessionCatalogBatchAction,
    pub(crate) items: Vec<DesktopSessionCatalogBatchItemInput>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSessionCatalogBatchPlanSummary {
    pub(crate) plan_id: String,
    pub(crate) action: DesktopSessionCatalogBatchAction,
    pub(crate) generation: u64,
    pub(crate) total: usize,
    pub(crate) executable: usize,
    pub(crate) blocked: usize,
    pub(crate) items: Vec<DesktopSessionCatalogBatchPlanItemSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSessionCatalogBatchPlanItemSummary {
    pub(crate) session_ref: String,
    pub(crate) status: DesktopSessionCatalogBatchPlanStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSessionCatalogBatchReceiptSummary {
    pub(crate) plan_id: String,
    pub(crate) action: DesktopSessionCatalogBatchAction,
    pub(crate) total: usize,
    pub(crate) completed: usize,
    pub(crate) failed: usize,
    pub(crate) skipped: usize,
    pub(crate) items: Vec<DesktopSessionCatalogBatchReceiptItemSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSessionCatalogBatchReceiptItemSummary {
    pub(crate) session_ref: String,
    pub(crate) outcome: DesktopSessionCatalogBatchOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) quarantine_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) projection_generation: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSessionMutationSummary {
    pub(crate) session_ref: String,
    pub(crate) session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) projection_generation: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSessionQuarantineSummary {
    pub(crate) session_ref: String,
    pub(crate) quarantine_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) projection_generation: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSessionInvalidSourceDeleteSummary {
    pub(crate) session_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) projection_generation: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSessionSummary {
    pub(crate) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    pub(crate) run_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) foreground_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) route_transition: Option<DesktopSessionRouteTransitionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) route_recovery: Option<DesktopSessionRouteRecoverySummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSessionRouteTransitionSummary {
    pub(crate) kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) connection_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model_id: Option<String>,
    pub(crate) remote_context_reset: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopDurableFrontierSummary {
    pub(crate) through_stream_sequence: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopForegroundRunOwnerSummary {
    pub(crate) run_id: String,
    pub(crate) owner_revision: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationContinuity {
    pub(crate) durable_frontier: DesktopDurableFrontierSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) foreground_owner: Option<DesktopForegroundRunOwnerSummary>,
    pub(crate) retained_terminal_runs: Vec<DesktopRetainedTerminalRunSummary>,
    pub(crate) recovery_actions: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopRetainedTerminalRunSummary {
    pub(crate) run_id: String,
    pub(crate) terminal_tasks: Vec<DesktopTimelineTerminalTask>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationQueueView {
    pub(crate) schema_version: u16,
    pub(crate) session_id: String,
    pub(crate) generation: String,
    pub(crate) paused: bool,
    pub(crate) total_items: u32,
    pub(crate) items: Vec<DesktopConversationQueueItem>,
    pub(crate) truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next_dispatchable_entry_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationQueueItem {
    pub(crate) entry_id: String,
    pub(crate) order: u32,
    pub(crate) kind: &'static str,
    pub(crate) status: &'static str,
    pub(crate) prompt_preview: String,
    pub(crate) prompt_preview_truncated: bool,
    pub(crate) prompt_material: &'static str,
    pub(crate) dispatchable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) blocked_reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) created_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) updated_at_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopConversationQueueCommandInput {
    pub(crate) session_id: String,
    pub(crate) expected_generation: String,
    pub(crate) action: DesktopConversationQueueActionInput,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum DesktopConversationQueueActionInput {
    Enqueue {
        prompt: String,
        kind: DesktopConversationQueueItemKindInput,
        reasoning_effort: Option<DesktopReasoningEffort>,
    },
    Edit {
        entry_id: String,
        prompt: String,
        reasoning_effort: Option<DesktopReasoningEffort>,
    },
    Remove {
        entry_id: String,
    },
    Reorder {
        entry_id: String,
        after_entry_id: Option<String>,
    },
    Pause,
    Resume,
    InterruptAndRunNext {
        foreground_run_id: String,
        foreground_owner_revision: String,
    },
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopConversationQueueItemKindInput {
    Chat,
    PlanPrompt,
    AgentMention,
    AgentMessage,
    Unknown,
}

impl DesktopConversationQueueActionInput {
    pub(crate) fn into_native(self) -> NativeConversationQueueCommandAction {
        match self {
            Self::Enqueue {
                prompt,
                kind,
                reasoning_effort,
            } => NativeConversationQueueCommandAction::Enqueue {
                prompt,
                kind: kind.into(),
                reasoning_effort,
            },
            Self::Edit {
                entry_id,
                prompt,
                reasoning_effort,
            } => NativeConversationQueueCommandAction::Edit {
                entry_id,
                prompt,
                reasoning_effort,
            },
            Self::Remove { entry_id } => NativeConversationQueueCommandAction::Remove { entry_id },
            Self::Reorder {
                entry_id,
                after_entry_id,
            } => NativeConversationQueueCommandAction::Reorder {
                entry_id,
                after_entry_id,
            },
            Self::Pause => NativeConversationQueueCommandAction::Pause,
            Self::Resume => NativeConversationQueueCommandAction::Resume,
            Self::InterruptAndRunNext {
                foreground_run_id,
                foreground_owner_revision,
            } => NativeConversationQueueCommandAction::InterruptAndRunNext {
                foreground_run_id,
                foreground_owner_revision,
            },
        }
    }
}

impl From<DesktopConversationQueueItemKindInput> for NativeConversationQueueItemKind {
    fn from(value: DesktopConversationQueueItemKindInput) -> Self {
        match value {
            DesktopConversationQueueItemKindInput::Chat => Self::Chat,
            DesktopConversationQueueItemKindInput::PlanPrompt => Self::PlanPrompt,
            DesktopConversationQueueItemKindInput::AgentMention => Self::AgentMention,
            DesktopConversationQueueItemKindInput::AgentMessage => Self::AgentMessage,
            DesktopConversationQueueItemKindInput::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationQueueCommandReceipt {
    pub(crate) command_id: String,
    pub(crate) client_id: String,
    pub(crate) session_id: String,
    pub(crate) action: &'static str,
    pub(crate) expected_generation: String,
    pub(crate) generation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) interrupt_owner: Option<DesktopForegroundRunOwnerSummary>,
    pub(crate) queue: DesktopConversationQueueView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) correlation_id: Option<String>,
    pub(crate) replayed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationRecoveryView {
    pub(crate) checkpoints: Vec<DesktopCheckpointView>,
    pub(crate) fork_points: Vec<DesktopConversationForkPointView>,
    pub(crate) through_stream_sequence: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCompactionReview {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) preview_id: Option<String>,
    pub(crate) folded_event_count: usize,
    pub(crate) retained_event_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) policy: Option<DesktopCompactionPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) details: Option<DesktopCompactionDetails>,
    pub(crate) admission: DesktopCompactionAdmission,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCompactionPolicy {
    pub(crate) strategy: String,
    pub(crate) phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) forecast_confidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) admission_reason: Option<String>,
    pub(crate) native_carrier_available: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCompactionConstraint {
    pub(crate) text: String,
    pub(crate) source_event_id: String,
    pub(crate) source_field_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCompactionToolArtifact {
    pub(crate) source_event_id: String,
    pub(crate) content_sha256: String,
    pub(crate) tool_name: String,
    pub(crate) tool_call_id: String,
    pub(crate) status: String,
    pub(crate) original_content_bytes: u64,
    pub(crate) original_content_token_upper_bound: u64,
    pub(crate) head_excerpt: String,
    pub(crate) tail_excerpt: String,
    pub(crate) reason: String,
    pub(crate) recovery_instruction: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCompactionDetails {
    pub(crate) active_objective: String,
    pub(crate) objective_source_event_id: String,
    pub(crate) active_constraints: Vec<DesktopCompactionConstraint>,
    pub(crate) folded_complete_turn_count: usize,
    pub(crate) folded_token_upper_bound: u64,
    pub(crate) retained_complete_turn_count: usize,
    pub(crate) retained_token_upper_bound: u64,
    pub(crate) tool_artifact_count: usize,
    pub(crate) tool_artifacts: Vec<DesktopCompactionToolArtifact>,
    pub(crate) pending_work_count: usize,
    pub(crate) unresolved_question_count: usize,
    pub(crate) recoverable_attachment_count: usize,
    pub(crate) protected_control_event_count: usize,
    pub(crate) protected_active_tool_or_approval_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) current_cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) break_even_turns: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(crate) enum DesktopCompactionAdmission {
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCompactionEconomics {
    pub(crate) before_input_tokens: u64,
    pub(crate) target_input_tokens: u64,
    pub(crate) context_window_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) safety_buffer_tokens: u64,
    pub(crate) savings_tokens: u64,
    pub(crate) savings_ratio_ppm: u32,
    pub(crate) minimum_savings_tokens: u64,
    pub(crate) minimum_savings_ratio_ppm: u32,
    pub(crate) summary_cache_read_tokens: u64,
    pub(crate) summary_uncached_input_tokens: u64,
    pub(crate) summary_output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) summary_cost_nano_usd: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCheckpointView {
    pub(crate) checkpoint_id: String,
    pub(crate) checkpoint_digest: String,
    pub(crate) turn_index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) prompt: Option<String>,
    pub(crate) files: Vec<DesktopCheckpointFileView>,
    pub(crate) unknown_mutation_count: usize,
    pub(crate) fully_restorable: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCheckpointFileView {
    pub(crate) path: String,
    pub(crate) restore_kind: &'static str,
    pub(crate) availability: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationForkPointView {
    pub(crate) source_turn_index: usize,
    pub(crate) source_turn_digest: String,
    pub(crate) source_boundary_stream_sequence: u64,
    pub(crate) source_finalized_stream_sequence: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopCheckpointRestorePreviewInput {
    pub(crate) session_id: String,
    pub(crate) checkpoint_id: String,
    pub(crate) checkpoint_digest: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCheckpointRestoreReview {
    pub(crate) checkpoint_id: String,
    pub(crate) checkpoint_digest: String,
    pub(crate) files: Vec<DesktopCheckpointRestorePreviewFile>,
    pub(crate) reverse_diffs: Vec<DesktopCheckpointReverseDiff>,
    pub(crate) unknown_mutation_count: usize,
    pub(crate) ready: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCheckpointRestorePreviewFile {
    pub(crate) path: String,
    pub(crate) restore_kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) expected_current_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) actual_current_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) conflict_reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCheckpointReverseDiff {
    pub(crate) path: String,
    pub(crate) diff: String,
    pub(crate) truncated: bool,
    pub(crate) original_line_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopConversationRecoveryCommandInput {
    pub(crate) session_id: String,
    pub(crate) action: DesktopConversationRecoveryActionInput,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum DesktopConversationRecoveryActionInput {
    ApplyCompaction {
        preview_id: String,
    },
    RestoreCheckpoint {
        checkpoint_id: String,
        checkpoint_digest: String,
    },
    ForkConversation {
        source_turn_digest: String,
        model_ref: DesktopProviderModelRefInput,
    },
}

impl DesktopConversationRecoveryActionInput {
    pub(crate) fn into_native(self) -> NativeConversationRecoveryCommandAction {
        match self {
            Self::ApplyCompaction { preview_id } => {
                NativeConversationRecoveryCommandAction::ApplyCompaction { preview_id }
            }
            Self::RestoreCheckpoint {
                checkpoint_id,
                checkpoint_digest,
            } => NativeConversationRecoveryCommandAction::RestoreCheckpoint {
                checkpoint_id,
                checkpoint_digest,
            },
            Self::ForkConversation {
                source_turn_digest,
                model_ref,
            } => NativeConversationRecoveryCommandAction::ForkConversation {
                source_turn_digest,
                model_ref: sigil_desktop::DesktopProviderModelRef {
                    connection_id: model_ref.connection_id,
                    model_id: model_ref.model_id,
                },
            },
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationRecoveryCommandReceipt {
    pub(crate) command_id: String,
    pub(crate) client_id: String,
    pub(crate) session_id: String,
    pub(crate) action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) compaction: Option<DesktopCompactionReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) restore: Option<DesktopCheckpointRestoreReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fork: Option<DesktopConversationForkReceipt>,
    pub(crate) recovery: DesktopConversationRecoveryView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) correlation_id: Option<String>,
    pub(crate) replayed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCompactionReceipt {
    pub(crate) compaction_id: String,
    pub(crate) attempt_id: String,
    pub(crate) task_memory_id: String,
    pub(crate) folded_event_count: usize,
    pub(crate) tool_output_projection_recorded: bool,
    pub(crate) native_carrier_materialized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) native_carrier_status: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCompactionExecutionSummary {
    pub(crate) outcome: &'static str,
    pub(crate) recovery: DesktopConversationRecoveryView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) compaction: Option<DesktopCompactionReceipt>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCheckpointRestoreReceipt {
    pub(crate) checkpoint_id: String,
    pub(crate) batch_id: String,
    pub(crate) restored_file_count: usize,
    pub(crate) verification_stale: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationForkReceipt {
    pub(crate) session_ref: String,
    pub(crate) session_id: String,
    pub(crate) copied_message_count: usize,
    pub(crate) copied_external_provenance_count: usize,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopTranscriptRequest {
    pub(crate) before: Option<u64>,
    pub(crate) limit: Option<u16>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopTranscriptPage {
    pub(crate) total_messages: u64,
    pub(crate) messages: Vec<DesktopTranscriptMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next_before: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopTranscriptMessage {
    pub(crate) ordinal: u64,
    pub(crate) message_id: String,
    pub(crate) role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) assistant_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_name: Option<String>,
    pub(crate) image_attachment_count: u64,
    pub(crate) truncated: bool,
    pub(crate) original_content_bytes: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopConversationDisplayRequest {
    pub(crate) cursor: Option<String>,
    pub(crate) limit: Option<u16>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationDisplayPage {
    pub(crate) schema_version: u16,
    pub(crate) request_scope: String,
    pub(crate) through_session_stream_sequence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) terminal_frontier: Option<DesktopConversationTerminalFrontier>,
    pub(crate) total_items: String,
    pub(crate) items: Vec<DesktopConversationDisplayItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next_cursor: Option<String>,
    pub(crate) has_more: bool,
    pub(crate) gap_facts: Vec<DesktopConversationDisplayGapFact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) live_provisional_anchor: Option<DesktopConversationLiveProvisionalAnchor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) task_control: Option<DesktopConversationTaskControl>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) plan_review: Option<DesktopPlanReview>,
    pub(crate) user_inputs: Vec<DesktopUserInputRequestSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) user_input: Option<DesktopUserInputRequestSummary>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    tag = "kind",
    deny_unknown_fields
)]
pub(crate) enum DesktopToolArtifactSelector {
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopToolArtifactReadInput {
    pub(crate) artifact_ref: String,
    pub(crate) selector: DesktopToolArtifactSelector,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopToolArtifactPage {
    pub(crate) schema_version: u16,
    pub(crate) request_scope: String,
    pub(crate) artifact_ref: String,
    pub(crate) selector: DesktopToolArtifactSelector,
    pub(crate) body: String,
    pub(crate) body_encoding: &'static str,
    pub(crate) returned_bytes: u32,
    pub(crate) page_sha256: String,
    pub(crate) artifact_sha256: String,
    pub(crate) eof: bool,
    pub(crate) match_count: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next_selector: Option<DesktopToolArtifactSelector>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationTaskControl {
    pub(crate) schema_version: u16,
    pub(crate) task_id: String,
    pub(crate) phase: &'static str,
    pub(crate) status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) execution: Option<DesktopTimelineTaskExecutionBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) plan_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) plan_status: Option<String>,
    pub(crate) steps: Vec<DesktopConversationTaskPlanStep>,
    pub(crate) steps_truncated: bool,
    pub(crate) checklist: Vec<DesktopTimelineTaskChecklistItem>,
    pub(crate) active_children: u32,
    pub(crate) completed_children: u32,
    pub(crate) failed_children: u32,
    pub(crate) lanes: Vec<DesktopConversationTaskLane>,
    pub(crate) lanes_truncated: bool,
    pub(crate) can_continue: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationTaskPlanStep {
    pub(crate) step_id: String,
    pub(crate) title: String,
    pub(crate) role: String,
    pub(crate) depends_on: Vec<String>,
    pub(crate) mode: String,
    pub(crate) isolation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationTaskLane {
    pub(crate) lane_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) plan_id: Option<String>,
    pub(crate) status: String,
    pub(crate) conflicts: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationTerminalFrontier {
    pub(crate) run_id: String,
    pub(crate) session_stream_sequence: String,
    pub(crate) status: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationDisplayItem {
    pub(crate) schema_version: u16,
    pub(crate) display_id: String,
    pub(crate) display_order: DesktopConversationDisplayOrder,
    pub(crate) source_event_id: String,
    pub(crate) kind: &'static str,
    pub(crate) source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) run_sequence: Option<String>,
    pub(crate) status: &'static str,
    pub(crate) content: DesktopConversationDisplayContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reconciles: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationDisplayOrder {
    pub(crate) session_stream_sequence: String,
    pub(crate) subindex: u32,
}

#[derive(Debug, Serialize)]
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub(crate) enum DesktopConversationDisplayContent {
    Message {
        role: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        skill: Option<DesktopConversationDisplaySkillReference>,
        #[serde(skip_serializing_if = "Option::is_none")]
        assistant_phase: Option<&'static str>,
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
        #[serde(skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        truncated: bool,
        original_content_bytes: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        artifact_ref: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        artifact_availability: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        observed_bytes: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        persisted_bytes: Option<u64>,
        has_more: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        preview_truncated: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        truncation_reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        capture_completeness: Option<String>,
    },
    Approval {
        call_id: String,
        tool_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        decision: Option<&'static str>,
    },
    Checkpoint {
        outcome: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        checkpoint_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        conflict_reason: Option<&'static str>,
    },
    Notice {
        text: String,
        truncated: bool,
        original_content_bytes: u64,
    },
    Terminal {
        #[serde(skip_serializing_if = "Option::is_none")]
        final_message_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        safe_summary: Option<String>,
        summary_truncated: bool,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationDisplaySkillReference {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationDisplayGapFact {
    pub(crate) kind: &'static str,
    pub(crate) after_session_stream_sequence: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConversationLiveProvisionalAnchor {
    pub(crate) durable_frontier: String,
    pub(crate) run_id: String,
    pub(crate) run_sequence: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopRunStartInput {
    pub(crate) session_id: String,
    pub(crate) prompt: String,
    pub(crate) permission_mode: DesktopPermissionMode,
    pub(crate) model_ref: Option<DesktopProviderModelRefSummary>,
    pub(crate) model_selection_binding: Option<String>,
    pub(crate) route_recovery_binding: Option<String>,
    pub(crate) reasoning_effort: Option<DesktopReasoningEffort>,
    pub(crate) reasoning_effort_binding: Option<String>,
    pub(crate) skill_binding: Option<DesktopSkillBindingInput>,
    pub(crate) agent_binding: Option<DesktopAgentBindingInput>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopTaskContinuationInput {
    pub(crate) session_id: String,
    pub(crate) task_id: String,
    pub(crate) guidance: Option<String>,
    pub(crate) permission_mode: DesktopPermissionMode,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopSkillBindingInput {
    pub(crate) skill_id: String,
    pub(crate) skill_sha256: String,
    pub(crate) index_fingerprint: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopAgentBindingInput {
    pub(crate) profile_id: String,
    pub(crate) snapshot_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopRunAttachInput {
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    pub(crate) owner_revision: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopRunSummary {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) status: &'static str,
    pub(crate) permission_mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<&'static str>,
    pub(crate) stream_sequence: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopRunContext {
    pub(crate) model_ref: DesktopProviderModelRefSummary,
    pub(crate) provider_name: String,
    pub(crate) model_name: String,
    pub(crate) model_options: Vec<DesktopModelOption>,
    pub(crate) model_selection: &'static str,
    pub(crate) model_selection_binding: String,
    pub(crate) default_permission_mode: &'static str,
    pub(crate) available_permission_modes: Vec<&'static str>,
    pub(crate) available_reasoning_efforts: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) default_reasoning_effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort_binding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) context_window_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_prompt_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache_usage: Option<DesktopCacheUsage>,
    pub(crate) context_window_source: &'static str,
    pub(crate) extension_catalog: DesktopExtensionCatalog,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) route_recovery: Option<DesktopSessionRouteRecoverySummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCacheUsage {
    pub(crate) cache_read_tokens: u64,
    pub(crate) cache_miss_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache_write_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_layout_mutation: Option<String>,
    pub(crate) provider_miss_without_local_mutation: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSessionRouteRecoverySummary {
    pub(crate) code: &'static str,
    pub(crate) allowed_actions: Vec<&'static str>,
    pub(crate) recovery_binding: String,
    pub(crate) retryable: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopAgentActivitySummary {
    pub(crate) total_agents: usize,
    pub(crate) active_agents: usize,
    pub(crate) terminal_agents: usize,
    pub(crate) items: Vec<DesktopAgentActivityItemSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopAgentActivityItemSummary {
    pub(crate) thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) profile_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) display_name: Option<String>,
    pub(crate) objective: String,
    pub(crate) status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    pub(crate) handoff_status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result_summary: Option<String>,
    pub(crate) result_summary_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) usage: Option<DesktopAgentUsageSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopAgentUsageSummary {
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) total_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cached_tokens: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopModelOption {
    pub(crate) model_ref: DesktopProviderModelRefSummary,
    pub(crate) display_name: String,
    pub(crate) availability: String,
    pub(crate) recommendation: String,
    pub(crate) provenance: String,
    pub(crate) model_name: String,
    pub(crate) available_reasoning_efforts: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) default_reasoning_effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort_binding: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopExtensionCatalog {
    pub(crate) commands: Vec<DesktopCommandCatalogEntry>,
    pub(crate) skills: Vec<DesktopSkillCatalogEntry>,
    pub(crate) agents: Vec<DesktopAgentCatalogEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCommandCatalogEntry {
    pub(crate) canonical: String,
    pub(crate) aliases: Vec<String>,
    pub(crate) label: String,
    pub(crate) description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) argument_hint: Option<String>,
    pub(crate) completes_with_space: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) client_action: Option<&'static str>,
    pub(crate) available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unavailable_reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSkillBinding {
    pub(crate) skill_id: String,
    pub(crate) skill_sha256: String,
    pub(crate) index_fingerprint: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSkillCatalogEntry {
    pub(crate) id: String,
    pub(crate) invocation_token: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) source: String,
    pub(crate) run_mode: String,
    pub(crate) trust: String,
    pub(crate) available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) binding: Option<DesktopSkillBinding>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopAgentCatalogEntry {
    pub(crate) id: String,
    pub(crate) invocation_token: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) source: String,
    pub(crate) kind: String,
    pub(crate) trust: String,
    pub(crate) enabled: bool,
    pub(crate) user_invocable: bool,
    pub(crate) available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) binding: Option<DesktopAgentBindingSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopAgentBindingSummary {
    pub(crate) profile_id: String,
    pub(crate) snapshot_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopRunAttachment {
    pub(crate) run: DesktopRunSummary,
    pub(crate) events: Vec<DesktopTimelineEvent>,
    pub(crate) stream_state: DesktopRunStreamState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stream_message: Option<&'static str>,
    pub(crate) has_gap: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopRunCancelInput {
    pub(crate) session_id: String,
    pub(crate) run_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopTerminalTaskCancelInput {
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    pub(crate) task_id: String,
    pub(crate) expected_generation: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopTerminalTaskCancelSummary {
    pub(crate) command_id: String,
    pub(crate) client_id: String,
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) correlation_id: Option<String>,
    pub(crate) terminal_task: DesktopTimelineTerminalTask,
    pub(crate) replayed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopTaskPauseInput {
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    pub(crate) task_id: String,
    pub(crate) execution: DesktopTaskExecutionBindingInput,
}

#[derive(Debug, Deserialize)]
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    tag = "kind",
    deny_unknown_fields
)]
pub(crate) enum DesktopTaskExecutionBindingInput {
    Plan { plan_version: u32 },
    Direct { admission_id: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopApprovalDecisionInput {
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    pub(crate) call_id: String,
    pub(crate) approval_request_id: String,
    pub(crate) tool_call_hash: String,
    pub(crate) policy_version: String,
    pub(crate) expires_at_ms: u64,
    pub(crate) decision: DesktopApprovalActionInput,
    #[serde(default, rename = "familyPattern")]
    pub(crate) family_pattern: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopApprovalActionInput {
    ApproveOnce,
    ApproveSession,
    ApproveFamily,
    Deny,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopApprovalDecisionSummary {
    pub(crate) command_id: String,
    pub(crate) client_id: String,
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    pub(crate) call_id: String,
    pub(crate) approval_request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) expected_stream_sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) correlation_id: Option<String>,
    pub(crate) decision: &'static str,
    pub(crate) route_state: &'static str,
    pub(crate) registry_revision: u64,
    pub(crate) replayed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopPlanDecisionInput {
    pub(crate) session_id: String,
    pub(crate) plan_id: String,
    pub(crate) expected_plan_hash: String,
    pub(crate) action: DesktopPlanDecisionActionInput,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopPlanDecisionActionInput {
    Run,
    Save,
    Revise,
    Reject,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPlanDecisionSummary {
    pub(crate) command_id: String,
    pub(crate) client_id: String,
    pub(crate) session_id: String,
    pub(crate) plan_id: String,
    pub(crate) plan_hash: String,
    pub(crate) action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) task_id: Option<String>,
    /// RFC-0067: durable Task phase right after admission.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) task_phase: Option<sigil_desktop::DesktopTaskExecutionPhase>,
    /// RFC-0067: typed blocker when admission held the Task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) task_blocker: Option<sigil_desktop::DesktopTaskBlocker>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) revision_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) user_input_request: Option<DesktopUserInputRequestSummary>,
    pub(crate) replayed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPlanReview {
    pub(crate) plan_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) plan_hash: Option<String>,
    pub(crate) status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) summary: Option<String>,
    pub(crate) summary_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) step_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) target_path_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_check_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) risk: Option<String>,
    pub(crate) allowed_actions: Vec<&'static str>,
    pub(crate) source: &'static str,
    pub(crate) stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) revision: Option<DesktopPlanRevisionSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPlanRevisionSummary {
    pub(crate) request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) attempt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) attempt_ordinal: Option<u32>,
    pub(crate) status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) terminal_reason: Option<String>,
}

impl From<sigil_desktop::DesktopPlanReview> for DesktopPlanReview {
    fn from(value: sigil_desktop::DesktopPlanReview) -> Self {
        Self {
            plan_id: value.plan_id,
            plan_hash: value.plan_hash,
            status: match value.status {
                sigil_desktop::DesktopPlanReviewStatus::Started => "started",
                sigil_desktop::DesktopPlanReviewStatus::WaitingForInput => "waiting_for_input",
                sigil_desktop::DesktopPlanReviewStatus::Finalizing => "finalizing",
                sigil_desktop::DesktopPlanReviewStatus::DraftReady => "draft_ready",
                sigil_desktop::DesktopPlanReviewStatus::CompileFailed => "compile_failed",
                sigil_desktop::DesktopPlanReviewStatus::CompletedWithoutDraft => {
                    "completed_without_draft"
                }
                sigil_desktop::DesktopPlanReviewStatus::Blocked => "blocked",
                sigil_desktop::DesktopPlanReviewStatus::Paused => "paused",
                sigil_desktop::DesktopPlanReviewStatus::Failed => "failed",
                sigil_desktop::DesktopPlanReviewStatus::Interrupted => "interrupted",
                sigil_desktop::DesktopPlanReviewStatus::Cancelled => "cancelled",
            },
            summary: value.summary,
            summary_truncated: value.summary_truncated,
            step_count: value.step_count,
            target_path_count: value.target_path_count,
            suggested_check_count: value.suggested_check_count,
            risk: value.risk,
            allowed_actions: value
                .allowed_actions
                .into_iter()
                .map(|action| match action {
                    sigil_desktop::DesktopPlanAction::Run => "run",
                    sigil_desktop::DesktopPlanAction::Save => "save",
                    sigil_desktop::DesktopPlanAction::Revise => "revise",
                    sigil_desktop::DesktopPlanAction::Reject => "reject",
                })
                .collect(),
            source: match value.source {
                sigil_desktop::DesktopPlanReviewSource::ExplicitPlanCommand => {
                    "explicit_plan_command"
                }
                sigil_desktop::DesktopPlanReviewSource::AutomaticConversationRoute => {
                    "automatic_conversation_route"
                }
            },
            stale: value.stale,
            revision: value.revision.map(|revision| DesktopPlanRevisionSummary {
                request_id: revision.request_id,
                attempt_id: revision.attempt_id,
                attempt_ordinal: revision.attempt_ordinal,
                status: match revision.status {
                    sigil_desktop::DesktopPlanRevisionStatus::AwaitingGuidance => {
                        "awaiting_guidance"
                    }
                    sigil_desktop::DesktopPlanRevisionStatus::Queued => "queued",
                    sigil_desktop::DesktopPlanRevisionStatus::Researching => "researching",
                    sigil_desktop::DesktopPlanRevisionStatus::WaitingForInput => {
                        "waiting_for_input"
                    }
                    sigil_desktop::DesktopPlanRevisionStatus::Finalizing => "finalizing",
                    sigil_desktop::DesktopPlanRevisionStatus::Failed => "failed",
                    sigil_desktop::DesktopPlanRevisionStatus::Cancelled => "cancelled",
                    sigil_desktop::DesktopPlanRevisionStatus::Succeeded => "succeeded",
                },
                terminal_reason: revision.terminal_reason,
            }),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopVerificationRerunBinding {
    pub(crate) request_id: String,
    pub(crate) task_id: String,
    pub(crate) plan_version: u32,
    pub(crate) step_id: String,
    pub(crate) check_spec_id: String,
    pub(crate) check_spec_hash: String,
    pub(crate) policy_hash: String,
    pub(crate) workspace_snapshot_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopVerificationRerunInput {
    pub(crate) session_id: String,
    pub(crate) request: DesktopVerificationRerunBinding,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopVerificationSummary {
    pub(crate) task_id: String,
    pub(crate) step_id: String,
    pub(crate) scope_kind: &'static str,
    pub(crate) scope_id: String,
    pub(crate) verdict: &'static str,
    pub(crate) status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) recommended_check_spec_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) recommendation_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) recommendation_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) action: Option<DesktopVerificationActionSummary>,
    pub(crate) evidence: DesktopVerificationEvidenceSummary,
}

#[derive(Debug, Serialize)]
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub(crate) enum DesktopVerificationActionSummary {
    Rerun {
        request: DesktopVerificationRerunBinding,
    },
    ReviewApproval {
        check_spec_id: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopVerificationEvidenceSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) check_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) check_spec_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) check_status: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) receipt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) workspace_snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) changeset_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) changeset_apply_event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) command_event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output_artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) failure_summary: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopTaskIntegrationReviewBinding {
    pub(crate) request_id: String,
    pub(crate) task_id: String,
    pub(crate) plan_id: String,
    pub(crate) plan_version: u32,
    pub(crate) preview_digest: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopTaskIntegrationAcceptInput {
    pub(crate) session_id: String,
    pub(crate) request: DesktopTaskIntegrationReviewBinding,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopTaskIntegrationLaneSummary {
    pub(crate) lane_id: String,
    pub(crate) candidate_kind: &'static str,
    pub(crate) proposal_count: usize,
    pub(crate) verification_receipt_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopTaskIntegrationReviewSummary {
    pub(crate) schema_version: u16,
    pub(crate) request: DesktopTaskIntegrationReviewBinding,
    pub(crate) aggregate_diff: String,
    pub(crate) aggregate_diff_digest: String,
    pub(crate) preview_digest: String,
    pub(crate) policy_digest: String,
    pub(crate) target_kind: &'static str,
    pub(crate) lanes: Vec<DesktopTaskIntegrationLaneSummary>,
    pub(crate) child_verification_receipt_count: usize,
    pub(crate) lane_verification_receipt_count: usize,
    pub(crate) conflict_reasons: Vec<String>,
    pub(crate) verification_invalidation_count: usize,
    pub(crate) parent_verification_pending: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopTaskIntegrationAcceptanceSummary {
    pub(crate) request: DesktopTaskIntegrationReviewBinding,
    pub(crate) promotion_status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parent_verdict: Option<&'static str>,
    pub(crate) can_continue: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) promotion_cleanup_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parent_cleanup_error: Option<String>,
}

impl From<DesktopSessionCatalogState> for DesktopCatalogState {
    fn from(value: DesktopSessionCatalogState) -> Self {
        match value {
            DesktopSessionCatalogState::Ready => Self::Ready,
            DesktopSessionCatalogState::Oversized => Self::Oversized,
            DesktopSessionCatalogState::ScanBudgetExceeded => Self::ScanBudgetExceeded,
            DesktopSessionCatalogState::Invalid => Self::Invalid,
        }
    }
}

impl From<DesktopSessionCatalogSourceDiagnostic> for DesktopCatalogSourceDiagnostic {
    fn from(value: DesktopSessionCatalogSourceDiagnostic) -> Self {
        match value {
            DesktopSessionCatalogSourceDiagnostic::UnsafeSource => Self::UnsafeSource,
            DesktopSessionCatalogSourceDiagnostic::InvalidEventStream => Self::InvalidEventStream,
            DesktopSessionCatalogSourceDiagnostic::InvalidProjection => Self::InvalidProjection,
            DesktopSessionCatalogSourceDiagnostic::MissingSessionIdentity => {
                Self::MissingSessionIdentity
            }
        }
    }
}

impl From<DesktopSessionCatalogPage> for DesktopCatalogPage {
    fn from(value: DesktopSessionCatalogPage) -> Self {
        Self {
            workspace_id: value.workspace_id,
            generation: value.generation,
            reconciled_at_unix_ms: value.reconciled_at_unix_ms,
            degraded_source_count: value.degraded_source_count,
            identity_conflict_count: value.identity_conflict_count,
            truncated_source_count: value.truncated_source_count,
            entries: value.entries.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

impl From<DesktopSessionCatalogBatchPlan> for DesktopSessionCatalogBatchPlanSummary {
    fn from(value: DesktopSessionCatalogBatchPlan) -> Self {
        Self {
            plan_id: value.plan_id,
            action: value.action,
            generation: value.generation,
            total: value.total,
            executable: value.executable,
            blocked: value.blocked,
            items: value
                .items
                .into_iter()
                .map(|item| DesktopSessionCatalogBatchPlanItemSummary {
                    session_ref: item.session_ref,
                    status: item.status,
                    reason: item.reason,
                })
                .collect(),
        }
    }
}

impl From<DesktopSessionCatalogBatchReceipt> for DesktopSessionCatalogBatchReceiptSummary {
    fn from(value: DesktopSessionCatalogBatchReceipt) -> Self {
        Self {
            plan_id: value.plan_id,
            action: value.action,
            total: value.total,
            completed: value.completed,
            failed: value.failed,
            skipped: value.skipped,
            items: value
                .items
                .into_iter()
                .map(|item| DesktopSessionCatalogBatchReceiptItemSummary {
                    session_ref: item.session_ref,
                    outcome: item.outcome,
                    reason: item.reason,
                    operation_id: item.operation_id,
                    quarantine_name: item.quarantine_name,
                    projection_generation: item.projection_generation,
                })
                .collect(),
        }
    }
}

impl From<DesktopSessionCatalogEntry> for DesktopCatalogEntry {
    fn from(value: DesktopSessionCatalogEntry) -> Self {
        Self {
            session_ref: value.session_ref,
            session_id: value.session_id,
            source_state: value.source_state.into(),
            source_diagnostic: value.source_diagnostic.map(Into::into),
            source_bytes: value.source_bytes,
            source_modified_at_unix_ms: value.source_modified_at_unix_ms,
            provider_name: value.provider_name,
            model_name: value.model_name,
            title: value.title,
            user_message_count: value.user_message_count,
            assistant_message_count: value.assistant_message_count,
            tool_result_count: value.tool_result_count,
            pinned: value.pinned,
        }
    }
}

impl From<DesktopSessionSnapshot> for DesktopSessionSummary {
    fn from(value: DesktopSessionSnapshot) -> Self {
        Self {
            id: value.id,
            label: value.label,
            run_count: value.run_ids.len(),
            foreground_run_id: value.foreground_run_id,
            route_transition: value.route_transition.map(|transition| {
                DesktopSessionRouteTransitionSummary {
                    kind: match transition.kind {
                        DesktopSessionRouteTransitionKind::Exact => "exact",
                        DesktopSessionRouteTransitionKind::Rebound => "rebound",
                        DesktopSessionRouteTransitionKind::ExplicitlyConfirmed => {
                            "explicitly_confirmed"
                        }
                    },
                    connection_id: transition.connection_id,
                    model_id: transition.model_id,
                    remote_context_reset: transition.remote_context_reset,
                }
            }),
            route_recovery: value
                .route_recovery
                .map(desktop_session_route_recovery_summary),
        }
    }
}

pub(crate) fn desktop_session_route_recovery_summary(
    recovery: DesktopSessionRouteRecoveryView,
) -> DesktopSessionRouteRecoverySummary {
    DesktopSessionRouteRecoverySummary {
        code: match recovery.code {
            DesktopSessionRouteRecoveryCode::SessionRouteConfirmationRequired => {
                "session_route_confirmation_required"
            }
            DesktopSessionRouteRecoveryCode::SessionRouteSelectionRequired => {
                "session_route_selection_required"
            }
            DesktopSessionRouteRecoveryCode::ModelRouteNotConfigured => {
                "model_route_not_configured"
            }
            DesktopSessionRouteRecoveryCode::ConnectionConfigInvalid => "connection_config_invalid",
            DesktopSessionRouteRecoveryCode::ProviderUnavailable => "provider_unavailable",
            DesktopSessionRouteRecoveryCode::SessionAlreadyActive => "session_already_active",
            DesktopSessionRouteRecoveryCode::SessionWriterBusy => "session_writer_busy",
            DesktopSessionRouteRecoveryCode::SessionStreamInvalid => "session_stream_invalid",
        },
        allowed_actions: recovery
            .allowed_actions
            .into_iter()
            .map(|action| match action {
                DesktopSessionRouteRecoveryAction::ConfirmCurrentRoute => "confirm_current_route",
                DesktopSessionRouteRecoveryAction::RepairConnection => "repair_connection",
                DesktopSessionRouteRecoveryAction::SelectReplacement => "select_replacement",
                DesktopSessionRouteRecoveryAction::StartNewSession => "start_new_session",
                DesktopSessionRouteRecoveryAction::RetryProvider => "retry_provider",
                DesktopSessionRouteRecoveryAction::RetrySessionAttach => "retry_session_attach",
                DesktopSessionRouteRecoveryAction::BackToSessionLibrary => {
                    "back_to_session_library"
                }
            })
            .collect(),
        recovery_binding: recovery.recovery_binding,
        retryable: recovery.retryable,
    }
}

impl TryFrom<sigil_desktop::DesktopSessionContinuityView> for DesktopConversationContinuity {
    type Error = sigil_desktop::DesktopProtocolEventError;

    fn try_from(value: sigil_desktop::DesktopSessionContinuityView) -> Result<Self, Self::Error> {
        Ok(Self {
            durable_frontier: DesktopDurableFrontierSummary {
                through_stream_sequence: value.durable_frontier.through_stream_sequence,
            },
            foreground_owner: value.foreground_owner.map(|owner| {
                DesktopForegroundRunOwnerSummary {
                    run_id: owner.run_id,
                    owner_revision: owner.owner_revision,
                }
            }),
            retained_terminal_runs: value
                .retained_terminal_runs
                .into_iter()
                .map(|run| {
                    Ok(DesktopRetainedTerminalRunSummary {
                        run_id: run.id,
                        terminal_tasks: run
                            .terminal_tasks
                            .iter()
                            .map(DesktopTimelineTerminalTask::try_from)
                            .collect::<Result<Vec<_>, _>>()?,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            recovery_actions: value
                .recovery_actions
                .into_iter()
                .map(|action| match action {
                    sigil_desktop::DesktopContinuityRecoveryAction::RetryCurrent => "retry_current",
                    sigil_desktop::DesktopContinuityRecoveryAction::OpenAnotherWorkspace => {
                        "open_another_workspace"
                    }
                    sigil_desktop::DesktopContinuityRecoveryAction::OpenDiagnostics => {
                        "open_diagnostics"
                    }
                    sigil_desktop::DesktopContinuityRecoveryAction::ShowDetails => "show_details",
                    sigil_desktop::DesktopContinuityRecoveryAction::ContinueReadOnly => {
                        "continue_read_only"
                    }
                })
                .collect(),
        })
    }
}

impl From<NativeConversationQueueView> for DesktopConversationQueueView {
    fn from(value: NativeConversationQueueView) -> Self {
        Self {
            schema_version: value.schema_version,
            session_id: value.session_id,
            generation: value.generation.0,
            paused: value.paused,
            total_items: value.total_items,
            items: value.items.into_iter().map(Into::into).collect(),
            truncated: value.truncated,
            next_dispatchable_entry_id: value.next_dispatchable_entry_id,
        }
    }
}

impl From<NativeConversationQueueItem> for DesktopConversationQueueItem {
    fn from(value: NativeConversationQueueItem) -> Self {
        Self {
            entry_id: value.entry_id,
            order: value.order,
            kind: conversation_queue_item_kind_label(value.kind),
            status: conversation_queue_item_status_label(value.status),
            prompt_preview: value.prompt_preview,
            prompt_preview_truncated: value.prompt_preview_truncated,
            prompt_material: conversation_queue_prompt_material_label(value.prompt_material),
            dispatchable: value.dispatchable,
            blocked_reason: value
                .blocked_reason
                .map(conversation_queue_blocked_reason_label),
            created_at_ms: value.created_at_ms,
            updated_at_ms: value.updated_at_ms,
        }
    }
}

impl From<NativeConversationQueueCommandReceipt> for DesktopConversationQueueCommandReceipt {
    fn from(value: NativeConversationQueueCommandReceipt) -> Self {
        Self {
            command_id: value.command_id,
            client_id: value.client_id,
            session_id: value.session_id,
            action: conversation_queue_action_kind_label(value.action),
            expected_generation: value.expected_generation.0,
            generation: value.generation.0,
            interrupt_owner: value
                .interrupt_owner
                .map(|owner| DesktopForegroundRunOwnerSummary {
                    run_id: owner.run_id,
                    owner_revision: owner.owner_revision,
                }),
            queue: value.queue.into(),
            correlation_id: value.correlation_id,
            replayed: value.replayed,
        }
    }
}

fn conversation_queue_item_kind_label(value: NativeConversationQueueItemKind) -> &'static str {
    match value {
        NativeConversationQueueItemKind::Chat => "chat",
        NativeConversationQueueItemKind::PlanPrompt => "plan_prompt",
        NativeConversationQueueItemKind::AgentMention => "agent_mention",
        NativeConversationQueueItemKind::AgentMessage => "agent_message",
        NativeConversationQueueItemKind::Unknown => "unknown",
    }
}

fn conversation_queue_item_status_label(
    value: sigil_desktop::DesktopConversationQueueItemStatus,
) -> &'static str {
    match value {
        sigil_desktop::DesktopConversationQueueItemStatus::Queued => "queued",
        sigil_desktop::DesktopConversationQueueItemStatus::Dispatching => "dispatching",
        sigil_desktop::DesktopConversationQueueItemStatus::Delivered => "delivered",
        sigil_desktop::DesktopConversationQueueItemStatus::Rejected => "rejected",
        sigil_desktop::DesktopConversationQueueItemStatus::Cancelled => "cancelled",
        sigil_desktop::DesktopConversationQueueItemStatus::Stale => "stale",
        sigil_desktop::DesktopConversationQueueItemStatus::Unknown => "unknown",
    }
}

fn conversation_queue_prompt_material_label(
    value: sigil_desktop::DesktopConversationQueuePromptMaterial,
) -> &'static str {
    match value {
        sigil_desktop::DesktopConversationQueuePromptMaterial::PersistedSafe => "persisted_safe",
        sigil_desktop::DesktopConversationQueuePromptMaterial::AvailableProcessLocal => {
            "available_process_local"
        }
        sigil_desktop::DesktopConversationQueuePromptMaterial::RequiresReentry => {
            "requires_reentry"
        }
    }
}

fn conversation_queue_blocked_reason_label(
    value: sigil_desktop::DesktopConversationQueueBlockedReason,
) -> &'static str {
    match value {
        sigil_desktop::DesktopConversationQueueBlockedReason::QueuePaused => "queue_paused",
        sigil_desktop::DesktopConversationQueueBlockedReason::RequiresReentry => "requires_reentry",
        sigil_desktop::DesktopConversationQueueBlockedReason::ForegroundRunActive => {
            "foreground_run_active"
        }
        sigil_desktop::DesktopConversationQueueBlockedReason::WaitingForTerminalFrontier => {
            "waiting_for_terminal_frontier"
        }
        sigil_desktop::DesktopConversationQueueBlockedReason::ForegroundOwnerLost => {
            "foreground_owner_lost"
        }
        sigil_desktop::DesktopConversationQueueBlockedReason::PermissionRequired => {
            "permission_required"
        }
        sigil_desktop::DesktopConversationQueueBlockedReason::Conflict => "conflict",
        sigil_desktop::DesktopConversationQueueBlockedReason::Stale => "stale",
        sigil_desktop::DesktopConversationQueueBlockedReason::Terminal => "terminal",
        sigil_desktop::DesktopConversationQueueBlockedReason::UnsupportedTarget => {
            "unsupported_target"
        }
        sigil_desktop::DesktopConversationQueueBlockedReason::MaterialUnavailable => {
            "material_unavailable"
        }
    }
}

fn conversation_queue_action_kind_label(
    value: NativeConversationQueueCommandActionKind,
) -> &'static str {
    match value {
        NativeConversationQueueCommandActionKind::Enqueue => "enqueue",
        NativeConversationQueueCommandActionKind::Edit => "edit",
        NativeConversationQueueCommandActionKind::Remove => "remove",
        NativeConversationQueueCommandActionKind::Reorder => "reorder",
        NativeConversationQueueCommandActionKind::Pause => "pause",
        NativeConversationQueueCommandActionKind::Resume => "resume",
        NativeConversationQueueCommandActionKind::InterruptAndRunNext => "interrupt_and_run_next",
    }
}

impl From<NativeConversationRecoveryView> for DesktopConversationRecoveryView {
    fn from(value: NativeConversationRecoveryView) -> Self {
        Self {
            checkpoints: value
                .checkpoints
                .into_iter()
                .map(|checkpoint| DesktopCheckpointView {
                    checkpoint_id: checkpoint.checkpoint_id,
                    checkpoint_digest: checkpoint.checkpoint_digest,
                    turn_index: checkpoint.turn_index,
                    prompt: checkpoint.prompt,
                    files: checkpoint
                        .files
                        .into_iter()
                        .map(|file| DesktopCheckpointFileView {
                            path: file.path,
                            restore_kind: checkpoint_restore_kind_label(file.restore_kind),
                            availability: checkpoint_availability_label(file.availability),
                        })
                        .collect(),
                    unknown_mutation_count: checkpoint.unknown_mutation_count,
                    fully_restorable: checkpoint.fully_restorable,
                })
                .collect(),
            fork_points: value
                .fork_points
                .into_iter()
                .map(|point| DesktopConversationForkPointView {
                    source_turn_index: point.source_turn_index,
                    source_turn_digest: point.source_turn_digest,
                    source_boundary_stream_sequence: point.source_boundary_stream_sequence,
                    source_finalized_stream_sequence: point.source_finalized_stream_sequence,
                })
                .collect(),
            through_stream_sequence: value.through_stream_sequence,
        }
    }
}

impl From<NativeCompactionReview> for DesktopCompactionReview {
    fn from(value: NativeCompactionReview) -> Self {
        Self {
            preview_id: value.preview_id,
            folded_event_count: value.folded_event_count,
            retained_event_count: value.retained_event_count,
            policy: value.policy.map(|policy| DesktopCompactionPolicy {
                strategy: policy.strategy,
                phase: policy.phase,
                forecast_confidence: policy.forecast_confidence,
                admission_reason: policy.admission_reason,
                native_carrier_available: policy.native_carrier_available,
            }),
            details: value.details.map(|details| DesktopCompactionDetails {
                active_objective: details.active_objective,
                objective_source_event_id: details.objective_source_event_id,
                active_constraints: details
                    .active_constraints
                    .into_iter()
                    .map(|constraint| DesktopCompactionConstraint {
                        text: constraint.text,
                        source_event_id: constraint.source_event_id,
                        source_field_path: constraint.source_field_path,
                    })
                    .collect(),
                folded_complete_turn_count: details.folded_complete_turn_count,
                folded_token_upper_bound: details.folded_token_upper_bound,
                retained_complete_turn_count: details.retained_complete_turn_count,
                retained_token_upper_bound: details.retained_token_upper_bound,
                tool_artifact_count: details.tool_artifact_count,
                tool_artifacts: details
                    .tool_artifacts
                    .into_iter()
                    .map(|artifact| DesktopCompactionToolArtifact {
                        source_event_id: artifact.source_event_id,
                        content_sha256: artifact.content_sha256,
                        tool_name: artifact.tool_name,
                        tool_call_id: artifact.tool_call_id,
                        status: artifact.status,
                        original_content_bytes: artifact.original_content_bytes,
                        original_content_token_upper_bound: artifact
                            .original_content_token_upper_bound,
                        head_excerpt: artifact.head_excerpt,
                        tail_excerpt: artifact.tail_excerpt,
                        reason: artifact.reason,
                        recovery_instruction: artifact.recovery_instruction,
                    })
                    .collect(),
                pending_work_count: details.pending_work_count,
                unresolved_question_count: details.unresolved_question_count,
                recoverable_attachment_count: details.recoverable_attachment_count,
                protected_control_event_count: details.protected_control_event_count,
                protected_active_tool_or_approval_count: details
                    .protected_active_tool_or_approval_count,
                current_cache_read_tokens: details.current_cache_read_tokens,
                break_even_turns: details.break_even_turns,
            }),
            admission: match value.admission {
                NativeCompactionAdmission::Prepared {
                    standalone_tool_output_shrink_available,
                } => DesktopCompactionAdmission::Prepared {
                    standalone_tool_output_shrink_available,
                },
                NativeCompactionAdmission::Ready { economics } => {
                    DesktopCompactionAdmission::Ready {
                        economics: DesktopCompactionEconomics {
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
                NativeCompactionAdmission::NoFoldableHistory {
                    durable_message_count,
                    minimum_tail_turn_count,
                } => DesktopCompactionAdmission::NoFoldableHistory {
                    durable_message_count,
                    minimum_tail_turn_count,
                },
                NativeCompactionAdmission::Unavailable { reason } => {
                    DesktopCompactionAdmission::Unavailable { reason }
                }
            },
        }
    }
}

impl From<NativeCheckpointRestoreReview> for DesktopCheckpointRestoreReview {
    fn from(value: NativeCheckpointRestoreReview) -> Self {
        Self {
            checkpoint_id: value.checkpoint_id,
            checkpoint_digest: value.checkpoint_digest,
            files: value
                .files
                .into_iter()
                .map(|file| DesktopCheckpointRestorePreviewFile {
                    path: file.path,
                    restore_kind: checkpoint_restore_kind_label(file.restore_kind),
                    expected_current_hash: file.expected_current_hash,
                    actual_current_hash: file.actual_current_hash,
                    conflict_reason: file.conflict_reason.map(checkpoint_conflict_reason_label),
                })
                .collect(),
            reverse_diffs: value
                .reverse_diffs
                .into_iter()
                .map(|diff| DesktopCheckpointReverseDiff {
                    path: diff.path,
                    diff: diff.diff,
                    truncated: diff.truncated,
                    original_line_count: diff.original_line_count,
                })
                .collect(),
            unknown_mutation_count: value.unknown_mutation_count,
            ready: value.ready,
        }
    }
}

impl From<NativeConversationRecoveryCommandReceipt> for DesktopConversationRecoveryCommandReceipt {
    fn from(value: NativeConversationRecoveryCommandReceipt) -> Self {
        Self {
            command_id: value.command_id,
            client_id: value.client_id,
            session_id: value.session_id,
            action: conversation_recovery_action_kind_label(value.action),
            compaction: value.compaction.map(|receipt| DesktopCompactionReceipt {
                compaction_id: receipt.compaction_id,
                attempt_id: receipt.attempt_id,
                task_memory_id: receipt.task_memory_id,
                folded_event_count: receipt.folded_event_count,
                tool_output_projection_recorded: receipt.tool_output_projection_recorded,
                native_carrier_materialized: receipt.native_carrier_materialized,
                native_carrier_status: receipt.native_carrier_status,
            }),
            restore: value
                .restore
                .map(|receipt| DesktopCheckpointRestoreReceipt {
                    checkpoint_id: receipt.checkpoint_id,
                    batch_id: receipt.batch_id,
                    restored_file_count: receipt.restored_file_count,
                    verification_stale: receipt.verification_stale,
                }),
            fork: value.fork.map(|receipt| DesktopConversationForkReceipt {
                session_ref: receipt.session_ref,
                session_id: receipt.session_id,
                copied_message_count: receipt.copied_message_count,
                copied_external_provenance_count: receipt.copied_external_provenance_count,
            }),
            recovery: value.recovery.into(),
            correlation_id: value.correlation_id,
            replayed: value.replayed,
        }
    }
}

fn checkpoint_restore_kind_label(
    value: sigil_desktop::DesktopCheckpointRestoreKind,
) -> &'static str {
    match value {
        sigil_desktop::DesktopCheckpointRestoreKind::RestoreContent => "restore_content",
        sigil_desktop::DesktopCheckpointRestoreKind::RemoveCreatedFile => "remove_created_file",
    }
}

fn checkpoint_availability_label(
    value: sigil_desktop::DesktopCheckpointFileAvailability,
) -> &'static str {
    match value {
        sigil_desktop::DesktopCheckpointFileAvailability::Restorable => "restorable",
        sigil_desktop::DesktopCheckpointFileAvailability::Sensitive => "sensitive",
        sigil_desktop::DesktopCheckpointFileAvailability::Unsupported => "unsupported",
        sigil_desktop::DesktopCheckpointFileAvailability::Unavailable => "unavailable",
    }
}

fn checkpoint_conflict_reason_label(
    value: sigil_desktop::DesktopCheckpointRestoreConflictReason,
) -> &'static str {
    match value {
        sigil_desktop::DesktopCheckpointRestoreConflictReason::WorkspaceMismatch => {
            "workspace_mismatch"
        }
        sigil_desktop::DesktopCheckpointRestoreConflictReason::CurrentHashMismatch => {
            "current_hash_mismatch"
        }
        sigil_desktop::DesktopCheckpointRestoreConflictReason::IntentStateConflict => {
            "intent_state_conflict"
        }
        sigil_desktop::DesktopCheckpointRestoreConflictReason::ArtifactUnavailable => {
            "artifact_unavailable"
        }
        sigil_desktop::DesktopCheckpointRestoreConflictReason::SensitiveSnapshot => {
            "sensitive_snapshot"
        }
        sigil_desktop::DesktopCheckpointRestoreConflictReason::UnsupportedSnapshot => {
            "unsupported_snapshot"
        }
        sigil_desktop::DesktopCheckpointRestoreConflictReason::InvalidBinding => "invalid_binding",
    }
}

fn conversation_recovery_action_kind_label(
    value: NativeConversationRecoveryCommandActionKind,
) -> &'static str {
    match value {
        NativeConversationRecoveryCommandActionKind::PrepareCompaction => "prepare_compaction",
        NativeConversationRecoveryCommandActionKind::ApplyCompaction => "apply_compaction",
        NativeConversationRecoveryCommandActionKind::ApplyStandaloneToolOutputShrink => {
            "apply_standalone_tool_output_shrink"
        }
        NativeConversationRecoveryCommandActionKind::RestoreCheckpoint => "restore_checkpoint",
        NativeConversationRecoveryCommandActionKind::ForkConversation => "fork_conversation",
    }
}

impl From<DesktopSessionTranscriptPage> for DesktopTranscriptPage {
    fn from(value: DesktopSessionTranscriptPage) -> Self {
        Self {
            total_messages: value.total_messages,
            messages: value.messages.into_iter().map(Into::into).collect(),
            next_before: value.next_before,
        }
    }
}

impl From<DesktopSessionTranscriptMessage> for DesktopTranscriptMessage {
    fn from(value: DesktopSessionTranscriptMessage) -> Self {
        Self {
            ordinal: value.ordinal,
            message_id: value.message_id,
            role: match value.role {
                DesktopTranscriptRole::User => "user",
                DesktopTranscriptRole::Assistant => "assistant",
                DesktopTranscriptRole::Tool => "tool",
            },
            content: value.content,
            assistant_kind: value.assistant_kind.map(|kind| match kind {
                DesktopTranscriptAssistantKind::ToolPreamble => "tool_preamble",
                DesktopTranscriptAssistantKind::Progress => "progress",
                DesktopTranscriptAssistantKind::ReasoningTrace => "reasoning_trace",
                DesktopTranscriptAssistantKind::FinalAnswer => "final_answer",
            }),
            tool_name: value.tool_name,
            image_attachment_count: value.image_attachment_count,
            truncated: value.truncated,
            original_content_bytes: value.original_content_bytes,
        }
    }
}

impl From<NativeConversationDisplayPage> for DesktopConversationDisplayPage {
    fn from(value: NativeConversationDisplayPage) -> Self {
        Self {
            schema_version: value.schema_version,
            request_scope: value.request_scope,
            through_session_stream_sequence: value.through_session_stream_sequence,
            terminal_frontier: value.terminal_frontier.map(|frontier| {
                DesktopConversationTerminalFrontier {
                    run_id: frontier.run_id,
                    session_stream_sequence: frontier.session_stream_sequence,
                    status: conversation_display_status(frontier.status),
                }
            }),
            total_items: value.total_items,
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
            has_more: value.has_more,
            gap_facts: value
                .gap_facts
                .into_iter()
                .map(|fact| DesktopConversationDisplayGapFact {
                    kind: match fact.kind {
                        sigil_desktop::DesktopConversationDisplayGapKind::Retention => "retention",
                        sigil_desktop::DesktopConversationDisplayGapKind::Replay => "replay",
                    },
                    after_session_stream_sequence: fact.after_session_stream_sequence,
                })
                .collect(),
            live_provisional_anchor: value.live_provisional_anchor.map(|anchor| {
                DesktopConversationLiveProvisionalAnchor {
                    durable_frontier: anchor.durable_frontier,
                    run_id: anchor.run_id,
                    run_sequence: anchor.run_sequence,
                }
            }),
            task_control: value.task_control.map(Into::into),
            plan_review: value.plan_review.map(Into::into),
            user_inputs: value.user_inputs.into_iter().map(Into::into).collect(),
            user_input: value.user_input.map(Into::into),
        }
    }
}

impl From<NativeToolArtifactPage> for DesktopToolArtifactPage {
    fn from(value: NativeToolArtifactPage) -> Self {
        Self {
            schema_version: value.schema_version,
            request_scope: value.request_scope,
            artifact_ref: value.artifact_ref,
            selector: value.selector.into(),
            body: value.body,
            body_encoding: match value.body_encoding {
                NativeToolArtifactPageEncoding::Utf8 => "utf8",
                NativeToolArtifactPageEncoding::Base64 => "base64",
            },
            returned_bytes: value.returned_bytes,
            page_sha256: value.page_sha256,
            artifact_sha256: value.artifact_sha256,
            eof: value.eof,
            match_count: value.match_count,
            next_selector: value.next_selector.map(Into::into),
        }
    }
}

impl From<NativeToolArtifactSelector> for DesktopToolArtifactSelector {
    fn from(value: NativeToolArtifactSelector) -> Self {
        match value {
            NativeToolArtifactSelector::ByteSlice { offset, limit } => {
                Self::ByteSlice { offset, limit }
            }
            NativeToolArtifactSelector::LinePage {
                start_line,
                line_count,
            } => Self::LinePage {
                start_line,
                line_count,
            },
            NativeToolArtifactSelector::SearchLiteral {
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

impl From<NativeConversationTaskControl> for DesktopConversationTaskControl {
    fn from(value: NativeConversationTaskControl) -> Self {
        Self {
            schema_version: value.schema_version,
            task_id: value.task_id,
            phase: match value.phase {
                DesktopPublicTaskPhase::Routing => "routing",
                DesktopPublicTaskPhase::Planning => "planning",
                DesktopPublicTaskPhase::Execution => "execution",
                DesktopPublicTaskPhase::Integration => "integration",
                DesktopPublicTaskPhase::Synthesis => "synthesis",
                DesktopPublicTaskPhase::Terminal => "terminal",
            },
            status: value.status,
            execution: value.execution.map(|execution| match execution {
                sigil_desktop::DesktopTaskExecutionBinding::Plan { plan_version } => {
                    DesktopTimelineTaskExecutionBinding::Plan { plan_version }
                }
                sigil_desktop::DesktopTaskExecutionBinding::Direct { admission_id } => {
                    DesktopTimelineTaskExecutionBinding::Direct { admission_id }
                }
            }),
            plan_version: value.plan_version,
            plan_status: value.plan_status,
            steps: value.steps.into_iter().map(Into::into).collect(),
            steps_truncated: value.steps_truncated,
            checklist: value
                .checklist
                .into_iter()
                .map(|item| DesktopTimelineTaskChecklistItem {
                    item_id: item.item_id,
                    text: item.text,
                    status: item.status,
                })
                .collect(),
            active_children: value.active_children,
            completed_children: value.completed_children,
            failed_children: value.failed_children,
            lanes: value.lanes.into_iter().map(Into::into).collect(),
            lanes_truncated: value.lanes_truncated,
            can_continue: value.can_continue,
        }
    }
}

impl From<NativeConversationTaskPlanStep> for DesktopConversationTaskPlanStep {
    fn from(value: NativeConversationTaskPlanStep) -> Self {
        Self {
            step_id: value.step_id,
            title: value.title,
            role: value.role,
            depends_on: value.depends_on,
            mode: value.mode,
            isolation: value.isolation,
            status: value.status,
        }
    }
}

impl From<NativeConversationTaskLane> for DesktopConversationTaskLane {
    fn from(value: NativeConversationTaskLane) -> Self {
        Self {
            lane_id: value.lane_id,
            plan_id: value.plan_id,
            status: value.status,
            conflicts: value.conflicts,
        }
    }
}

impl From<NativeConversationDisplayItem> for DesktopConversationDisplayItem {
    fn from(value: NativeConversationDisplayItem) -> Self {
        Self {
            schema_version: value.schema_version,
            display_id: value.display_id,
            display_order: DesktopConversationDisplayOrder {
                session_stream_sequence: value.display_order.session_stream_sequence,
                subindex: value.display_order.subindex,
            },
            source_event_id: value.source_event_id,
            kind: match value.kind {
                NativeConversationDisplayItemKind::UserMessage => "user_message",
                NativeConversationDisplayItemKind::Reasoning => "reasoning",
                NativeConversationDisplayItemKind::AssistantMessage => "assistant_message",
                NativeConversationDisplayItemKind::Tool => "tool",
                NativeConversationDisplayItemKind::Approval => "approval",
                NativeConversationDisplayItemKind::Checkpoint => "checkpoint",
                NativeConversationDisplayItemKind::Notice => "notice",
                NativeConversationDisplayItemKind::Terminal => "terminal",
            },
            source: match value.source {
                NativeConversationDisplaySource::DurableTranscript => "durable_transcript",
                NativeConversationDisplaySource::DurableRunEvent => "durable_run_event",
                NativeConversationDisplaySource::LiveTransient => "live_transient",
            },
            run_id: value.run_id,
            run_sequence: value.run_sequence,
            status: conversation_display_status(value.status),
            content: value.content.into(),
            reconciles: value.reconciles,
        }
    }
}

impl From<NativeConversationDisplayContent> for DesktopConversationDisplayContent {
    fn from(value: NativeConversationDisplayContent) -> Self {
        match value {
            NativeConversationDisplayContent::Message {
                role,
                text,
                skill,
                assistant_phase,
                image_attachment_count,
                truncated,
                original_content_bytes,
            } => Self::Message {
                role: match role {
                    NativeConversationDisplayMessageRole::User => "user",
                    NativeConversationDisplayMessageRole::Assistant => "assistant",
                },
                text,
                skill: skill.map(|skill| DesktopConversationDisplaySkillReference {
                    id: skill.id,
                    name: skill.name,
                }),
                assistant_phase: assistant_phase.map(|phase| match phase {
                    NativeConversationDisplayAssistantPhase::ToolPreamble => "tool_preamble",
                    NativeConversationDisplayAssistantPhase::Progress => "progress",
                    NativeConversationDisplayAssistantPhase::FinalAnswer => "final_answer",
                }),
                image_attachment_count,
                truncated,
                original_content_bytes,
            },
            NativeConversationDisplayContent::Reasoning {
                text,
                truncated,
                original_content_bytes,
            } => Self::Reasoning {
                text,
                truncated,
                original_content_bytes,
            },
            NativeConversationDisplayContent::Tool {
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
                preview_truncated,
                truncation_reason,
                capture_completeness,
            } => Self::Tool {
                call_id,
                tool_name,
                output,
                truncated,
                original_content_bytes,
                artifact_ref,
                artifact_availability: artifact_availability.map(|availability| {
                    match availability {
                        NativeToolArtifactAvailability::Available => "available",
                        NativeToolArtifactAvailability::Expired => "expired",
                        NativeToolArtifactAvailability::Missing => "missing",
                        NativeToolArtifactAvailability::HashMismatch => "hash_mismatch",
                        NativeToolArtifactAvailability::PolicyRevoked => "policy_revoked",
                        NativeToolArtifactAvailability::Unavailable => "unavailable",
                    }
                    .to_owned()
                }),
                observed_bytes,
                persisted_bytes,
                has_more,
                preview_truncated: Some(preview_truncated),
                truncation_reason,
                capture_completeness,
            },
            NativeConversationDisplayContent::Approval {
                call_id,
                tool_name,
                decision,
            } => Self::Approval {
                call_id,
                tool_name,
                decision: decision.map(|decision| match decision {
                    NativeConversationDisplayApprovalDecision::Approved => "approved",
                    NativeConversationDisplayApprovalDecision::ApprovedForSession => {
                        "approved_for_session"
                    }
                    NativeConversationDisplayApprovalDecision::Denied => "denied",
                }),
            },
            NativeConversationDisplayContent::Checkpoint {
                outcome,
                checkpoint_id,
                conflict_reason,
            } => Self::Checkpoint {
                outcome: match outcome {
                    NativeConversationDisplayCheckpointOutcome::Restored => "restored",
                    NativeConversationDisplayCheckpointOutcome::Conflict => "conflict",
                },
                checkpoint_id,
                conflict_reason: conflict_reason.map(|reason| match reason {
                    NativeConversationDisplayCheckpointConflictReason::WorkspaceMismatch => {
                        "workspace_mismatch"
                    }
                    NativeConversationDisplayCheckpointConflictReason::CurrentHashMismatch => {
                        "current_hash_mismatch"
                    }
                    NativeConversationDisplayCheckpointConflictReason::IntentStateConflict => {
                        "intent_state_conflict"
                    }
                    NativeConversationDisplayCheckpointConflictReason::ArtifactUnavailable => {
                        "artifact_unavailable"
                    }
                    NativeConversationDisplayCheckpointConflictReason::SensitiveSnapshot => {
                        "sensitive_snapshot"
                    }
                    NativeConversationDisplayCheckpointConflictReason::UnsupportedSnapshot => {
                        "unsupported_snapshot"
                    }
                    NativeConversationDisplayCheckpointConflictReason::InvalidBinding => {
                        "invalid_binding"
                    }
                }),
            },
            NativeConversationDisplayContent::Notice {
                text,
                truncated,
                original_content_bytes,
            } => Self::Notice {
                text,
                truncated,
                original_content_bytes,
            },
            NativeConversationDisplayContent::Terminal {
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

fn conversation_display_status(status: NativeConversationDisplayStatus) -> &'static str {
    match status {
        NativeConversationDisplayStatus::Recorded => "recorded",
        NativeConversationDisplayStatus::Requested => "requested",
        NativeConversationDisplayStatus::WaitingForApproval => "waiting_for_approval",
        NativeConversationDisplayStatus::Approved => "approved",
        NativeConversationDisplayStatus::Denied => "denied",
        NativeConversationDisplayStatus::Completed => "completed",
        NativeConversationDisplayStatus::Succeeded => "succeeded",
        NativeConversationDisplayStatus::Failed => "failed",
        NativeConversationDisplayStatus::Cancelled => "cancelled",
        NativeConversationDisplayStatus::Interrupted => "interrupted",
        NativeConversationDisplayStatus::Paused => "paused",
        NativeConversationDisplayStatus::Blocked => "blocked",
        NativeConversationDisplayStatus::AwaitingUserInput => "awaiting_user_input",
    }
}

impl From<DesktopRunSnapshot> for DesktopRunSummary {
    fn from(value: DesktopRunSnapshot) -> Self {
        Self {
            id: value.id,
            session_id: value.session_id,
            status: match value.status {
                DesktopRunStatus::Starting => "starting",
                DesktopRunStatus::Running => "running",
                DesktopRunStatus::WaitingForApproval => "waiting_for_approval",
                DesktopRunStatus::CancelRequested => "cancel_requested",
                DesktopRunStatus::PauseRequested => "pause_requested",
                DesktopRunStatus::ExecutionUncertain => "execution_uncertain",
                DesktopRunStatus::Finished => "finished",
                DesktopRunStatus::Failed => "failed",
                DesktopRunStatus::Cancelled => "cancelled",
                DesktopRunStatus::Paused => "paused",
                DesktopRunStatus::Blocked => "blocked",
                DesktopRunStatus::Interrupted => "interrupted",
            },
            permission_mode: permission_mode_label(value.permission_mode),
            reasoning_effort: value.reasoning_effort.map(reasoning_effort_label),
            stream_sequence: value.stream_sequence,
        }
    }
}

impl From<DesktopRunContextView> for DesktopRunContext {
    fn from(value: DesktopRunContextView) -> Self {
        let extension_catalog = DesktopExtensionCatalog {
            commands: value
                .extension_catalog
                .commands
                .into_iter()
                .map(|entry| DesktopCommandCatalogEntry {
                    canonical: entry.canonical,
                    aliases: entry.aliases,
                    label: entry.label,
                    description: entry.description,
                    argument_hint: entry.argument_hint,
                    completes_with_space: entry.completes_with_space,
                    client_action: entry.client_action.map(application_client_action_label),
                    available: entry.available,
                    unavailable_reason: entry.unavailable_reason,
                })
                .collect(),
            skills: value
                .extension_catalog
                .skills
                .into_iter()
                .map(|entry| DesktopSkillCatalogEntry {
                    id: entry.id,
                    invocation_token: entry.invocation_token,
                    name: entry.name,
                    description: entry.description,
                    source: entry.source,
                    run_mode: entry.run_mode,
                    trust: entry.trust,
                    available: entry.available,
                    unavailable_reason: entry.unavailable_reason,
                    binding: entry.binding.map(|binding| DesktopSkillBinding {
                        skill_id: binding.skill_id,
                        skill_sha256: binding.skill_sha256,
                        index_fingerprint: binding.index_fingerprint,
                    }),
                })
                .collect(),
            agents: value
                .extension_catalog
                .agents
                .into_iter()
                .map(|entry| {
                    let name = agent_display_name(&entry.invocation_token, &entry.id);
                    DesktopAgentCatalogEntry {
                        id: entry.id,
                        invocation_token: entry.invocation_token,
                        name,
                        description: entry.description,
                        source: entry.source,
                        kind: entry.kind,
                        trust: entry.trust,
                        enabled: entry.enabled,
                        user_invocable: entry.user_invocable,
                        available: entry.available,
                        unavailable_reason: entry.unavailable_reason,
                        snapshot_id: entry.snapshot_id,
                        binding: entry.binding.map(|binding| DesktopAgentBindingSummary {
                            profile_id: binding.profile_id,
                            snapshot_id: binding.snapshot_id,
                        }),
                    }
                })
                .collect(),
        };
        Self {
            model_ref: DesktopProviderModelRefSummary {
                connection_id: value.model_ref.connection_id,
                model_id: value.model_ref.model_id,
            },
            provider_name: value.provider_name,
            model_name: value.model_name,
            model_options: value
                .model_options
                .into_iter()
                .map(|option| DesktopModelOption {
                    model_ref: DesktopProviderModelRefSummary {
                        connection_id: option.model_ref.connection_id,
                        model_id: option.model_ref.model_id,
                    },
                    display_name: option.display_name,
                    availability: option.availability,
                    recommendation: option.recommendation,
                    provenance: option.provenance,
                    model_name: option.model_name,
                    available_reasoning_efforts: option
                        .available_reasoning_efforts
                        .into_iter()
                        .map(reasoning_effort_label)
                        .collect(),
                    default_reasoning_effort: option
                        .default_reasoning_effort
                        .map(reasoning_effort_label),
                    reasoning_effort_binding: option.reasoning_effort_binding,
                })
                .collect(),
            model_selection: match value.model_selection {
                DesktopModelSelectionPolicy::SameSession => "same_session",
            },
            model_selection_binding: value.model_selection_binding,
            default_permission_mode: permission_mode_label(value.default_permission_mode),
            available_permission_modes: value
                .available_permission_modes
                .into_iter()
                .map(permission_mode_label)
                .collect(),
            available_reasoning_efforts: value
                .available_reasoning_efforts
                .into_iter()
                .map(reasoning_effort_label)
                .collect(),
            default_reasoning_effort: value.default_reasoning_effort.map(reasoning_effort_label),
            reasoning_effort_binding: value.reasoning_effort_binding,
            context_window_tokens: value.context_window_tokens,
            last_prompt_tokens: value.last_prompt_tokens,
            cache_usage: value.cache_usage.map(|usage| DesktopCacheUsage {
                cache_read_tokens: usage.cache_read_tokens,
                cache_miss_tokens: usage.cache_miss_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                last_layout_mutation: usage.last_layout_mutation,
                provider_miss_without_local_mutation: usage.provider_miss_without_local_mutation,
            }),
            context_window_source: match value.context_window_source {
                DesktopContextWindowSource::Connection => "connection",
                DesktopContextWindowSource::Provider => "provider",
                DesktopContextWindowSource::Config => "config",
                DesktopContextWindowSource::Unavailable => "unavailable",
            },
            extension_catalog,
            route_recovery: value.route_recovery.map(|recovery| {
                DesktopSessionRouteRecoverySummary {
                    code: match recovery.code {
                        DesktopSessionRouteRecoveryCode::SessionRouteConfirmationRequired => {
                            "session_route_confirmation_required"
                        }
                        DesktopSessionRouteRecoveryCode::SessionRouteSelectionRequired => {
                            "session_route_selection_required"
                        }
                        DesktopSessionRouteRecoveryCode::ModelRouteNotConfigured => {
                            "model_route_not_configured"
                        }
                        DesktopSessionRouteRecoveryCode::ConnectionConfigInvalid => {
                            "connection_config_invalid"
                        }
                        DesktopSessionRouteRecoveryCode::ProviderUnavailable => {
                            "provider_unavailable"
                        }
                        DesktopSessionRouteRecoveryCode::SessionAlreadyActive => {
                            "session_already_active"
                        }
                        DesktopSessionRouteRecoveryCode::SessionWriterBusy => "session_writer_busy",
                        DesktopSessionRouteRecoveryCode::SessionStreamInvalid => {
                            "session_stream_invalid"
                        }
                    },
                    allowed_actions: recovery
                        .allowed_actions
                        .into_iter()
                        .map(|action| match action {
                            DesktopSessionRouteRecoveryAction::ConfirmCurrentRoute => {
                                "confirm_current_route"
                            }
                            DesktopSessionRouteRecoveryAction::RepairConnection => {
                                "repair_connection"
                            }
                            DesktopSessionRouteRecoveryAction::SelectReplacement => {
                                "select_replacement"
                            }
                            DesktopSessionRouteRecoveryAction::StartNewSession => {
                                "start_new_session"
                            }
                            DesktopSessionRouteRecoveryAction::RetryProvider => "retry_provider",
                            DesktopSessionRouteRecoveryAction::RetrySessionAttach => {
                                "retry_session_attach"
                            }
                            DesktopSessionRouteRecoveryAction::BackToSessionLibrary => {
                                "back_to_session_library"
                            }
                        })
                        .collect(),
                    recovery_binding: recovery.recovery_binding,
                    retryable: recovery.retryable,
                }
            }),
        }
    }
}

fn agent_display_name(invocation_token: &str, fallback_id: &str) -> String {
    invocation_token
        .strip_prefix('@')
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(fallback_id)
        .to_owned()
}

impl From<DesktopAgentActivityView> for DesktopAgentActivitySummary {
    fn from(value: DesktopAgentActivityView) -> Self {
        Self {
            total_agents: value.total_agents,
            active_agents: value.active_agents,
            terminal_agents: value.terminal_agents,
            items: value
                .items
                .into_iter()
                .map(|item| DesktopAgentActivityItemSummary {
                    thread_id: item.thread_id,
                    profile_id: item.profile_id,
                    display_name: item.display_name,
                    objective: item.objective,
                    status: agent_activity_status_label(item.status),
                    reason: item.reason,
                    handoff_status: agent_handoff_status_label(item.handoff_status),
                    result_summary: item.result_summary,
                    result_summary_truncated: item.result_summary_truncated,
                    usage: item.usage.map(|usage| DesktopAgentUsageSummary {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        total_tokens: usage.total_tokens,
                        cached_tokens: usage.cached_tokens,
                    }),
                })
                .collect(),
        }
    }
}

fn agent_activity_status_label(status: DesktopAgentActivityStatus) -> &'static str {
    match status {
        DesktopAgentActivityStatus::Started => "started",
        DesktopAgentActivityStatus::Running => "running",
        DesktopAgentActivityStatus::Blocked => "blocked",
        DesktopAgentActivityStatus::Completed => "completed",
        DesktopAgentActivityStatus::Failed => "failed",
        DesktopAgentActivityStatus::Cancelled => "cancelled",
        DesktopAgentActivityStatus::Interrupted => "interrupted",
        DesktopAgentActivityStatus::Unavailable => "unavailable",
        DesktopAgentActivityStatus::Unknown => "unknown",
    }
}

fn agent_handoff_status_label(status: DesktopAgentHandoffStatus) -> &'static str {
    match status {
        DesktopAgentHandoffStatus::Pending => "pending",
        DesktopAgentHandoffStatus::ResultReady => "result_ready",
        DesktopAgentHandoffStatus::ResultRead => "result_read",
        DesktopAgentHandoffStatus::Returned => "returned",
        DesktopAgentHandoffStatus::Unavailable => "unavailable",
    }
}

fn application_client_action_label(value: DesktopApplicationClientAction) -> &'static str {
    match value {
        DesktopApplicationClientAction::PreviewCompaction => "preview_compaction",
        DesktopApplicationClientAction::OpenIntentStack => "open_intent_stack",
        DesktopApplicationClientAction::NewSession => "new_session",
        DesktopApplicationClientAction::FocusEffort => "focus_effort",
        DesktopApplicationClientAction::FocusModel => "focus_model",
        DesktopApplicationClientAction::OpenSessionPicker => "open_session_picker",
        DesktopApplicationClientAction::OpenAgentWorkbench => "open_agent_workbench",
        DesktopApplicationClientAction::OpenSettings => "open_settings",
        DesktopApplicationClientAction::OpenSupport => "open_support",
    }
}

fn permission_mode_label(value: DesktopPermissionMode) -> &'static str {
    match value {
        DesktopPermissionMode::ReadOnly => "read-only",
        DesktopPermissionMode::Manual => "manual",
        DesktopPermissionMode::AutoEdit => "auto-edit",
        DesktopPermissionMode::DangerFullAccess => "danger-full-access",
    }
}

fn reasoning_effort_label(value: DesktopReasoningEffort) -> &'static str {
    match value {
        DesktopReasoningEffort::Low => "low",
        DesktopReasoningEffort::Medium => "medium",
        DesktopReasoningEffort::High => "high",
        DesktopReasoningEffort::Max => "max",
    }
}

impl From<NativeApprovalCommandReceipt> for DesktopApprovalDecisionSummary {
    fn from(value: NativeApprovalCommandReceipt) -> Self {
        Self {
            command_id: value.command_id,
            client_id: value.client_id,
            session_id: value.session_id,
            run_id: value.run_id,
            call_id: value.call_id,
            approval_request_id: value.approval_request_id,
            expected_stream_sequence: value.expected_stream_sequence,
            correlation_id: value.correlation_id,
            decision: match value.decision.decision {
                sigil_desktop::DesktopApprovalRecordedDecision::Approved => "approved",
                sigil_desktop::DesktopApprovalRecordedDecision::ApprovedForSession => {
                    "approved_for_session"
                }
                sigil_desktop::DesktopApprovalRecordedDecision::Denied => "denied",
            },
            route_state: match value.route_state {
                sigil_desktop::DesktopApprovalRouteState::DecisionAccepted => "decision_accepted",
                sigil_desktop::DesktopApprovalRouteState::DeliveryUncertain => "delivery_uncertain",
                sigil_desktop::DesktopApprovalRouteState::Terminal => "terminal",
            },
            registry_revision: value.registry_revision,
            replayed: value.replayed,
        }
    }
}

impl From<DesktopVerificationRerunBinding> for DesktopVerificationRerunRequest {
    fn from(value: DesktopVerificationRerunBinding) -> Self {
        Self {
            request_id: value.request_id,
            task_id: value.task_id,
            plan_version: value.plan_version,
            step_id: value.step_id,
            check_spec_id: value.check_spec_id,
            check_spec_hash: value.check_spec_hash,
            policy_hash: value.policy_hash,
            workspace_snapshot_id: value.workspace_snapshot_id,
        }
    }
}

impl From<DesktopVerificationRerunRequest> for DesktopVerificationRerunBinding {
    fn from(value: DesktopVerificationRerunRequest) -> Self {
        Self {
            request_id: value.request_id,
            task_id: value.task_id,
            plan_version: value.plan_version,
            step_id: value.step_id,
            check_spec_id: value.check_spec_id,
            check_spec_hash: value.check_spec_hash,
            policy_hash: value.policy_hash,
            workspace_snapshot_id: value.workspace_snapshot_id,
        }
    }
}

impl From<DesktopVerificationView> for DesktopVerificationSummary {
    fn from(value: DesktopVerificationView) -> Self {
        let (scope_kind, scope_id) = match value.scope {
            DesktopVerificationScope::Run(id) => ("run", id),
            DesktopVerificationScope::Workspace(id) => ("workspace", id),
            DesktopVerificationScope::Task(id) => ("task", id),
            DesktopVerificationScope::Step(id) => ("step", id),
            DesktopVerificationScope::Agent(id) => ("agent", id),
            DesktopVerificationScope::Changeset(id) => ("changeset", id),
        };
        let action = value.action.map(|action| match action {
            DesktopVerificationAction::Rerun(request) => DesktopVerificationActionSummary::Rerun {
                request: request.into(),
            },
            DesktopVerificationAction::ReviewApproval { check_spec_id } => {
                DesktopVerificationActionSummary::ReviewApproval { check_spec_id }
            }
        });
        Self {
            task_id: value.task_id,
            step_id: value.step_id,
            scope_kind,
            scope_id,
            verdict: verification_verdict_label(value.verdict),
            status: value.status,
            recommended_check_spec_id: value.recommended_check_spec_id,
            recommendation_kind: value
                .recommendation_kind
                .map(verification_recommendation_kind_label),
            recommendation_reason: value.recommendation_reason,
            action,
            evidence: DesktopVerificationEvidenceSummary {
                check_run_id: value.evidence.check_run_id,
                check_spec_id: value.evidence.check_spec_id,
                check_status: value
                    .evidence
                    .check_status
                    .map(verification_check_status_label),
                receipt_id: value.evidence.receipt_id,
                workspace_snapshot_id: value.evidence.workspace_snapshot_id,
                changeset_id: value.evidence.changeset_id,
                changeset_apply_event_id: value.evidence.changeset_apply_event_id,
                command_event_id: value.evidence.command_event_id,
                output_artifact_id: value.evidence.output_artifact_id,
                failure_summary: value.evidence.failure_summary,
            },
        }
    }
}

impl From<DesktopTaskIntegrationReviewBinding> for DesktopTaskIntegrationReviewRequest {
    fn from(value: DesktopTaskIntegrationReviewBinding) -> Self {
        Self {
            request_id: value.request_id,
            task_id: value.task_id,
            plan_id: value.plan_id,
            plan_version: value.plan_version,
            preview_digest: value.preview_digest,
        }
    }
}

impl From<DesktopTaskIntegrationReviewRequest> for DesktopTaskIntegrationReviewBinding {
    fn from(value: DesktopTaskIntegrationReviewRequest) -> Self {
        Self {
            request_id: value.request_id,
            task_id: value.task_id,
            plan_id: value.plan_id,
            plan_version: value.plan_version,
            preview_digest: value.preview_digest,
        }
    }
}

impl From<DesktopTaskIntegrationReviewView> for DesktopTaskIntegrationReviewSummary {
    fn from(value: DesktopTaskIntegrationReviewView) -> Self {
        Self {
            schema_version: value.schema_version,
            request: value.request.into(),
            aggregate_diff: value.aggregate_diff,
            aggregate_diff_digest: value.aggregate_diff_digest,
            preview_digest: value.preview_digest,
            policy_digest: value.policy_digest,
            target_kind: integration_target_kind_label(value.target_kind),
            lanes: value
                .lanes
                .into_iter()
                .map(|lane| DesktopTaskIntegrationLaneSummary {
                    lane_id: lane.lane_id,
                    candidate_kind: integration_lane_kind_label(lane.candidate_kind),
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

impl From<DesktopTaskIntegrationAcceptanceView> for DesktopTaskIntegrationAcceptanceSummary {
    fn from(value: DesktopTaskIntegrationAcceptanceView) -> Self {
        Self {
            request: value.request.into(),
            promotion_status: integration_promotion_status_label(value.promotion_status),
            parent_verdict: value.parent_verdict.map(verification_verdict_label),
            can_continue: value.can_continue,
            promotion_cleanup_error: value.promotion_cleanup_error,
            parent_cleanup_error: value.parent_cleanup_error,
        }
    }
}

fn integration_target_kind_label(value: DesktopIntegrationPromotionTargetKind) -> &'static str {
    match value {
        DesktopIntegrationPromotionTargetKind::WorkspaceApply => "workspace_apply",
        DesktopIntegrationPromotionTargetKind::GitRefAdvance => "git_ref_advance",
    }
}

fn integration_lane_kind_label(value: DesktopIntegrationLaneCandidateKind) -> &'static str {
    match value {
        DesktopIntegrationLaneCandidateKind::ManagedRef => "managed_ref",
        DesktopIntegrationLaneCandidateKind::SnapshotWorkspace => "snapshot_workspace",
    }
}

fn integration_promotion_status_label(value: DesktopIntegrationPromotionStatus) -> &'static str {
    match value {
        DesktopIntegrationPromotionStatus::Prepared => "prepared",
        DesktopIntegrationPromotionStatus::Promoted => "promoted",
        DesktopIntegrationPromotionStatus::Conflict => "conflict",
        DesktopIntegrationPromotionStatus::Stale => "stale",
        DesktopIntegrationPromotionStatus::Failed => "failed",
        DesktopIntegrationPromotionStatus::Cancelled => "cancelled",
    }
}

fn verification_recommendation_kind_label(
    value: sigil_desktop::DesktopVerificationRecommendationKind,
) -> &'static str {
    match value {
        sigil_desktop::DesktopVerificationRecommendationKind::Run => "run",
        sigil_desktop::DesktopVerificationRecommendationKind::RerunNonWriting => {
            "rerun_non_writing"
        }
        sigil_desktop::DesktopVerificationRecommendationKind::Retry => "retry",
        sigil_desktop::DesktopVerificationRecommendationKind::ReviewApproval => "review_approval",
    }
}

fn verification_verdict_label(value: DesktopVerificationVerdict) -> &'static str {
    match value {
        DesktopVerificationVerdict::NotEvaluated => "not_evaluated",
        DesktopVerificationVerdict::NotApplicable => "not_applicable",
        DesktopVerificationVerdict::Pending => "pending",
        DesktopVerificationVerdict::Passed => "passed",
        DesktopVerificationVerdict::Failed => "failed",
        DesktopVerificationVerdict::Missing => "missing",
        DesktopVerificationVerdict::Inconclusive => "inconclusive",
        DesktopVerificationVerdict::Stale => "stale",
        DesktopVerificationVerdict::Skipped => "skipped",
    }
}

fn verification_check_status_label(value: DesktopVerificationCheckStatus) -> &'static str {
    match value {
        DesktopVerificationCheckStatus::Queued => "queued",
        DesktopVerificationCheckStatus::Running => "running",
        DesktopVerificationCheckStatus::Succeeded => "succeeded",
        DesktopVerificationCheckStatus::Failed => "failed",
        DesktopVerificationCheckStatus::Skipped => "skipped",
        DesktopVerificationCheckStatus::Inconclusive => "inconclusive",
        DesktopVerificationCheckStatus::Errored => "errored",
    }
}

#[cfg(test)]
#[path = "tests/ipc_tests.rs"]
mod tests;
