use serde::{Deserialize, Serialize};
use sigil_desktop::{
    DesktopRunSnapshot, DesktopRunStatus, DesktopSessionCatalogEntry, DesktopSessionCatalogPage,
    DesktopSessionCatalogState, DesktopSessionSnapshot, DesktopWorkspaceSummary,
};

use crate::recent::RecentWorkspaceSummary;

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopRunStartInput {
    pub(crate) session_id: String,
    pub(crate) prompt: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopRunSummary {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) status: &'static str,
    pub(crate) stream_sequence: u64,
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
