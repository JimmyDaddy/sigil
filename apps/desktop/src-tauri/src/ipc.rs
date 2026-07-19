use serde::{Deserialize, Serialize};
use sigil_desktop::{
    DesktopApprovalDecisionRecord, DesktopRunSnapshot, DesktopRunStatus,
    DesktopSessionCatalogEntry, DesktopSessionCatalogPage, DesktopSessionCatalogState,
    DesktopSessionSnapshot, DesktopSessionTranscriptMessage, DesktopSessionTranscriptPage,
    DesktopTimelineEvent, DesktopTranscriptAssistantKind, DesktopTranscriptRole,
    DesktopVerificationAction, DesktopVerificationCheckStatus, DesktopVerificationRerunRequest,
    DesktopVerificationScope, DesktopVerificationVerdict, DesktopVerificationView,
    DesktopWorkspaceSummary,
};

use crate::{recent::RecentWorkspaceSummary, run_streams::DesktopRunStreamState};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopBootstrap {
    pub(crate) protocol_version: u16,
    pub(crate) workspaces: Vec<DesktopWorkspaceSummary>,
    pub(crate) recent_workspaces: Vec<RecentWorkspaceSummary>,
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
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopSessionOpenInput {
    pub(crate) session_ref: String,
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) label: Option<String>,
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
    pub(crate) stream_sequence: u64,
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
    pub(crate) approve: bool,
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

impl From<DesktopSessionCatalogEntry> for DesktopCatalogEntry {
    fn from(value: DesktopSessionCatalogEntry) -> Self {
        Self {
            session_ref: value.session_ref,
            session_id: value.session_id,
            source_state: value.source_state.into(),
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
            stream_sequence: value.stream_sequence,
        }
    }
}

impl From<DesktopApprovalDecisionRecord> for DesktopApprovalDecisionSummary {
    fn from(value: DesktopApprovalDecisionRecord) -> Self {
        Self {
            run_id: value.run_id,
            call_id: value.call_id,
            decision: match value.decision {
                sigil_desktop::DesktopApprovalRecordedDecision::Approved => "approved",
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
