use serde::{Deserialize, Serialize};
use sigil_desktop::{
    DesktopAgentActivityStatus, DesktopAgentActivityView, DesktopAgentHandoffStatus,
    DesktopApplicationClientAction, DesktopApprovalDecisionRecord, DesktopContextWindowSource,
    DesktopModelSelectionPolicy, DesktopPermissionMode, DesktopReasoningEffort,
    DesktopRunContextView, DesktopRunSnapshot, DesktopRunStatus, DesktopSessionCatalogBatchAction,
    DesktopSessionCatalogBatchOutcome, DesktopSessionCatalogBatchPlan,
    DesktopSessionCatalogBatchPlanStatus, DesktopSessionCatalogBatchReceipt,
    DesktopSessionCatalogEntry, DesktopSessionCatalogPage, DesktopSessionCatalogState,
    DesktopSessionSnapshot, DesktopSessionTranscriptMessage, DesktopSessionTranscriptPage,
    DesktopSupportCheck, DesktopSupportDoctorReport, DesktopSupportEnvironment,
    DesktopSupportPrivacy, DesktopSupportStatus, DesktopSupportSummary, DesktopTimelineEvent,
    DesktopTranscriptAssistantKind, DesktopTranscriptRole, DesktopVerificationAction,
    DesktopVerificationCheckStatus, DesktopVerificationRerunRequest, DesktopVerificationScope,
    DesktopVerificationVerdict, DesktopVerificationView, DesktopWorkspaceSummary,
};

use crate::{
    appearance::{AppearanceSnapshot, ThemePreference},
    recent::RecentWorkspaceSummary,
    run_streams::DesktopRunStreamState,
};

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
    UnsupportedLegacy,
    Invalid,
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
    pub(crate) model_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopSessionOpenInput {
    pub(crate) session_ref: String,
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) label: Option<String>,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopRunStartInput {
    pub(crate) session_id: String,
    pub(crate) prompt: String,
    pub(crate) permission_mode: DesktopPermissionMode,
    pub(crate) model_name: Option<String>,
    pub(crate) model_selection_binding: Option<String>,
    pub(crate) reasoning_effort: Option<DesktopReasoningEffort>,
    pub(crate) reasoning_effort_binding: Option<String>,
    pub(crate) skill_binding: Option<DesktopSkillBindingInput>,
    pub(crate) agent_binding: Option<DesktopAgentBindingInput>,
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
    pub(crate) provider_name: String,
    pub(crate) model_name: String,
    pub(crate) available_models: Vec<String>,
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
    pub(crate) context_window_source: &'static str,
    pub(crate) extension_catalog: DesktopExtensionCatalog,
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
pub(crate) struct DesktopApprovalDecisionInput {
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    pub(crate) call_id: String,
    pub(crate) approval_request_id: String,
    pub(crate) tool_call_hash: String,
    pub(crate) policy_version: String,
    pub(crate) expires_at_ms: u64,
    pub(crate) decision: DesktopApprovalActionInput,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopApprovalActionInput {
    ApproveOnce,
    ApproveSession,
    Deny,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopApprovalDecisionSummary {
    pub(crate) run_id: String,
    pub(crate) call_id: String,
    pub(crate) decision: &'static str,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopVerificationRerunBinding {
    pub(crate) task_id: String,
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

impl From<DesktopSessionCatalogState> for DesktopCatalogState {
    fn from(value: DesktopSessionCatalogState) -> Self {
        match value {
            DesktopSessionCatalogState::Ready => Self::Ready,
            DesktopSessionCatalogState::Oversized => Self::Oversized,
            DesktopSessionCatalogState::ScanBudgetExceeded => Self::ScanBudgetExceeded,
            DesktopSessionCatalogState::UnsupportedLegacy => Self::UnsupportedLegacy,
            DesktopSessionCatalogState::Invalid => Self::Invalid,
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
        }
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
                DesktopRunStatus::ExecutionUncertain => "execution_uncertain",
                DesktopRunStatus::Finished => "finished",
                DesktopRunStatus::Failed => "failed",
                DesktopRunStatus::Cancelled => "cancelled",
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
                .map(|entry| DesktopAgentCatalogEntry {
                    id: entry.id,
                    invocation_token: entry.invocation_token,
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
                })
                .collect(),
        };
        Self {
            provider_name: value.provider_name,
            model_name: value.model_name,
            available_models: value.available_models,
            model_options: value
                .model_options
                .into_iter()
                .map(|option| DesktopModelOption {
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
                DesktopModelSelectionPolicy::PerRun => "per_run",
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
            context_window_source: match value.context_window_source {
                DesktopContextWindowSource::Provider => "provider",
                DesktopContextWindowSource::Config => "config",
                DesktopContextWindowSource::Unavailable => "unavailable",
            },
            extension_catalog,
        }
    }
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

impl From<DesktopApprovalDecisionRecord> for DesktopApprovalDecisionSummary {
    fn from(value: DesktopApprovalDecisionRecord) -> Self {
        Self {
            run_id: value.run_id,
            call_id: value.call_id,
            decision: match value.decision {
                sigil_desktop::DesktopApprovalRecordedDecision::Approved => "approved",
                sigil_desktop::DesktopApprovalRecordedDecision::ApprovedForSession => {
                    "approved_for_session"
                }
                sigil_desktop::DesktopApprovalRecordedDecision::Denied => "denied",
            },
        }
    }
}

impl From<DesktopVerificationRerunBinding> for DesktopVerificationRerunRequest {
    fn from(value: DesktopVerificationRerunBinding) -> Self {
        Self {
            task_id: value.task_id,
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
            task_id: value.task_id,
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
