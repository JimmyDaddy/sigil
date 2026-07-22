use serde::{Deserialize, Serialize};
use sigil_desktop::{
    DesktopAgentActivityStatus, DesktopAgentActivityView, DesktopAgentHandoffStatus,
    DesktopApplicationClientAction, DesktopApprovalDecisionRecord,
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
    DesktopConversationRecoveryView as NativeConversationRecoveryView, DesktopModelSelectionPolicy,
    DesktopPermissionMode, DesktopReasoningEffort, DesktopRunContextView, DesktopRunSnapshot,
    DesktopRunStatus, DesktopSessionCatalogBatchAction, DesktopSessionCatalogBatchOutcome,
    DesktopSessionCatalogBatchPlan, DesktopSessionCatalogBatchPlanStatus,
    DesktopSessionCatalogBatchReceipt, DesktopSessionCatalogEntry, DesktopSessionCatalogPage,
    DesktopSessionCatalogState, DesktopSessionSnapshot, DesktopSessionTranscriptMessage,
    DesktopSessionTranscriptPage, DesktopSupportCheck, DesktopSupportDoctorReport,
    DesktopSupportEnvironment, DesktopSupportPrivacy, DesktopSupportStatus, DesktopSupportSummary,
    DesktopTimelineEvent, DesktopTranscriptAssistantKind, DesktopTranscriptRole,
    DesktopVerificationAction, DesktopVerificationCheckStatus, DesktopVerificationRerunRequest,
    DesktopVerificationScope, DesktopVerificationVerdict, DesktopVerificationView,
    DesktopWorkspaceSummary,
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
    pub(crate) recovery_actions: Vec<&'static str>,
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
    pub(crate) admission: DesktopCompactionAdmission,
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(crate) enum DesktopCompactionAdmission {
    Ready {
        economics: DesktopCompactionEconomics,
    },
    NoFoldableHistory {
        durable_message_count: usize,
        configured_tail_message_count: usize,
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
            Self::ForkConversation { source_turn_digest } => {
                NativeConversationRecoveryCommandAction::ForkConversation { source_turn_digest }
            }
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

impl From<sigil_desktop::DesktopSessionContinuityView> for DesktopConversationContinuity {
    fn from(value: sigil_desktop::DesktopSessionContinuityView) -> Self {
        Self {
            durable_frontier: DesktopDurableFrontierSummary {
                through_stream_sequence: value.durable_frontier.through_stream_sequence,
            },
            foreground_owner: value.foreground_owner.map(|owner| {
                DesktopForegroundRunOwnerSummary {
                    run_id: owner.run_id,
                    owner_revision: owner.owner_revision,
                }
            }),
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
        }
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
            admission: match value.admission {
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
                        },
                    }
                }
                NativeCompactionAdmission::NoFoldableHistory {
                    durable_message_count,
                    configured_tail_message_count,
                } => DesktopCompactionAdmission::NoFoldableHistory {
                    durable_message_count,
                    configured_tail_message_count,
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
        NativeConversationRecoveryCommandActionKind::ApplyCompaction => "apply_compaction",
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
            } => Self::Tool {
                call_id,
                tool_name,
                output,
                truncated,
                original_content_bytes,
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
        NativeConversationDisplayStatus::Blocked => "blocked",
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
        DesktopApplicationClientAction::PreviewCompaction => "preview_compaction",
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
