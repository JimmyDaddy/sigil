//! Canonical, provider-neutral display projection for durable conversation history.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::Path,
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AssistantMessageKind, CheckpointRestoreConflictReason, CheckpointRestored, CompactionAppliedV2,
    ControlEntry, ConversationForked, ConversationInputPromotedEntry,
    ConversationRunLifecycleRecordV1, ConversationRunTerminalStatusV1, DurableEventType,
    JsonlSessionStore, MessageRole, ModelMessage, PublicRunEventKind, PublicTaskEventProjector,
    PublicTaskPhase, SessionLogEntry, SessionStreamRecord, ToolApprovalAuditAction,
    ToolApprovalUserDecision, ToolArtifactRefV1, ToolArtifactStore, TypedDomainEvent,
    UserInputLifecycleEntryV1, UserInputProjectionV1,
    conversation_run_lifecycle_record_from_stream, safe_persistence_text,
};
use thiserror::Error as ThisError;

/// Schema version for the canonical conversation display projection.
pub const CONVERSATION_DISPLAY_SCHEMA_VERSION: u16 = 1;
/// Default number of display items returned by one page.
pub const DEFAULT_CONVERSATION_DISPLAY_PAGE_SIZE: usize = 50;
/// Hard item limit for one display page.
pub const MAX_CONVERSATION_DISPLAY_PAGE_SIZE: usize = 100;
/// Hard safe-text limit for one projected item.
pub const MAX_CONVERSATION_DISPLAY_CONTENT_BYTES: usize = 64 * 1024;
/// Hard serialized-content budget for one projected page.
pub const MAX_CONVERSATION_DISPLAY_PAGE_BYTES: usize = 512 * 1024;
/// Hard item limit for one durable Task control collection.
pub const MAX_CONVERSATION_TASK_CONTROL_ITEMS: usize = 128;
/// Hard dependency/conflict detail limit within one durable Task control row.
pub const MAX_CONVERSATION_TASK_CONTROL_DETAIL_ITEMS: usize = 32;
/// Hard byte limit for one durable Task control step title.
pub const MAX_CONVERSATION_TASK_CONTROL_TITLE_BYTES: usize = 4 * 1024;
const MAX_CONVERSATION_DISPLAY_CURSOR_BYTES: usize = 4 * 1024;
const MAX_CONVERSATION_DISPLAY_IDENTITY_BYTES: usize = 512;

/// Stable failure classes for canonical conversation display projection.
#[derive(Debug, ThisError)]
pub enum ConversationDisplayProjectionError {
    /// The supplied cursor is malformed or belongs to another request scope.
    #[error("conversation display cursor is invalid: {source}")]
    InvalidCursor {
        #[source]
        source: anyhow::Error,
    },
    /// The cursor was valid when issued but its fixed durable frontier is no longer available.
    #[error("conversation display cursor is stale: {source}")]
    StaleCursor {
        #[source]
        source: anyhow::Error,
    },
    /// Durable projection could not be proven safely for a reason unrelated to cursor admission.
    #[error("conversation display projection is unavailable: {source}")]
    Unavailable {
        #[source]
        source: anyhow::Error,
    },
}

impl ConversationDisplayProjectionError {
    fn invalid_cursor(source: anyhow::Error) -> Self {
        Self::InvalidCursor { source }
    }

    fn stale_cursor(source: anyhow::Error) -> Self {
        Self::StaleCursor { source }
    }
}

impl From<anyhow::Error> for ConversationDisplayProjectionError {
    fn from(source: anyhow::Error) -> Self {
        Self::Unavailable { source }
    }
}

/// Durable ordering key. The stream sequence is authoritative; `subindex` is deterministic
/// within one source event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationDisplayOrderV1 {
    pub session_stream_sequence: u64,
    pub subindex: u32,
}

/// Provider-neutral visual category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplayItemKindV1 {
    UserMessage,
    Reasoning,
    AssistantMessage,
    Tool,
    Approval,
    Checkpoint,
    Notice,
    Terminal,
}

/// Evidence source used to build a display item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplaySourceV1 {
    DurableTranscript,
    DurableRunEvent,
    LiveTransient,
}

/// Bounded status vocabulary shared by display item kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplayStatusV1 {
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
    AwaitingUserInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplayMessageRoleV1 {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplayAssistantPhaseV1 {
    ToolPreamble,
    Progress,
    FinalAnswer,
}

/// User-selected skill bound to one durable prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationDisplaySkillReferenceV1 {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplayApprovalDecisionV1 {
    Approved,
    ApprovedForSession,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplayCheckpointOutcomeV1 {
    Restored,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplayCheckpointConflictReasonV1 {
    WorkspaceMismatch,
    CurrentHashMismatch,
    IntentStateConflict,
    ArtifactUnavailable,
    SensitiveSnapshot,
    UnsupportedSnapshot,
    InvalidBinding,
}

/// Stable semantic slot used to correlate one live protocol item with its durable successor.
///
/// The public identifier derived from this slot is a one-way digest; it never exposes the durable
/// session scope, run id, message id, or tool call id to a renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationLiveProvisionalSlotV1 {
    User,
    AssistantMessage { message_id: String },
    Tool { call_id: String },
    Approval { call_id: String },
    Terminal,
}

/// Typed, secret-safe content carried by one display item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", deny_unknown_fields)]
pub enum ConversationDisplayContentV1 {
    Message {
        role: ConversationDisplayMessageRoleV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skill: Option<ConversationDisplaySkillReferenceV1>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assistant_phase: Option<ConversationDisplayAssistantPhaseV1>,
        image_attachment_count: usize,
        truncated: bool,
        original_content_bytes: usize,
    },
    Reasoning {
        text: String,
        truncated: bool,
        original_content_bytes: usize,
    },
    Tool {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        truncated: bool,
        original_content_bytes: usize,
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
        #[serde(default)]
        preview_truncated: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        truncation_reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capture_completeness: Option<String>,
    },
    Approval {
        call_id: String,
        tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<ConversationDisplayApprovalDecisionV1>,
    },
    Checkpoint {
        outcome: ConversationDisplayCheckpointOutcomeV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conflict_reason: Option<ConversationDisplayCheckpointConflictReasonV1>,
    },
    Notice {
        text: String,
        truncated: bool,
        original_content_bytes: usize,
    },
    Terminal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_message_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        safe_summary: Option<String>,
        summary_truncated: bool,
    },
}

/// One canonical durable display item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationDisplayItemV1 {
    pub schema_version: u16,
    pub display_id: String,
    pub display_order: ConversationDisplayOrderV1,
    pub source_event_id: String,
    pub kind: ConversationDisplayItemKindV1,
    pub source: ConversationDisplaySourceV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_sequence: Option<u64>,
    pub status: ConversationDisplayStatusV1,
    pub content: ConversationDisplayContentV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciles: Option<Vec<String>>,
}

/// Latest proven terminal boundary at the page's fixed durable frontier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationTerminalFrontierV1 {
    pub run_id: String,
    pub session_stream_sequence: u64,
    pub status: ConversationDisplayStatusV1,
}

/// Bounded plan-step state needed to restore Task controls after an application restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationTaskPlanStepV1 {
    pub step_id: String,
    pub title: String,
    pub role: String,
    pub depends_on: Vec<String>,
    pub mode: String,
    pub isolation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Bounded integration-lane state without a private workspace, ref, path, or mutation authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationTaskLaneV1 {
    pub lane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    pub status: String,
    #[serde(default)]
    pub conflicts: Vec<String>,
}

/// Current durable Task state required by application control surfaces.
///
/// The projection deliberately omits the Task objective because it can contain raw user text.
/// It exposes only bounded public identities, plan summaries, counts, and renderer-safe status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationTaskControlV1 {
    pub schema_version: u16,
    pub task_id: String,
    pub phase: PublicTaskPhase,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_status: Option<String>,
    #[serde(default)]
    pub steps: Vec<ConversationTaskPlanStepV1>,
    pub steps_truncated: bool,
    pub active_children: u32,
    pub completed_children: u32,
    pub failed_children: u32,
    #[serde(default)]
    pub lanes: Vec<ConversationTaskLaneV1>,
    pub lanes_truncated: bool,
    pub can_continue: bool,
}

/// Opaque-cursor page over the canonical display projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationDisplayPageV1 {
    pub schema_version: u16,
    pub session_scope_id: String,
    pub through_session_stream_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_frontier: Option<ConversationTerminalFrontierV1>,
    pub total_items: u64,
    pub items: Vec<ConversationDisplayItemV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_control: Option<ConversationTaskControlV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_review: Option<sigil_kernel::PublicPlanReview>,
    /// Stable, oldest-first attention queue. `user_input` remains the first item for v1 clients.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_inputs: Vec<sigil_kernel::PublicUserInputRequestV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_input: Option<sigil_kernel::PublicUserInputRequestV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct ConversationDisplayCursorV1 {
    schema_version: u16,
    session_scope_sha256: String,
    through_session_stream_sequence: u64,
    frontier_binding_sha256: String,
    before_order: ConversationDisplayOrderV1,
}

#[derive(Debug, Clone)]
struct FixedFrontier {
    sequence: u64,
}

#[derive(Debug, Clone)]
struct ActiveRunProjection {
    run_id: String,
    final_message_id: Option<String>,
    user_provisional_reconciled: bool,
}

#[derive(Debug, Clone)]
struct ToolProjection {
    name: String,
    requested_display_id: String,
}

#[derive(Debug, Default)]
struct ConversationTaskControlProjection {
    events: PublicTaskEventProjector,
    tasks: BTreeMap<String, ConversationTaskControlV1>,
    current_task_id: Option<String>,
    focus_explicitly_selected: bool,
    task_run_scopes: BTreeMap<String, String>,
}

impl ConversationTaskControlProjection {
    fn apply_entry(&mut self, entry: &SessionLogEntry) {
        match entry {
            SessionLogEntry::User(_) => self.clear_current(),
            SessionLogEntry::Control(control) => self.apply_control(control),
            SessionLogEntry::Assistant(_)
            | SessionLogEntry::ToolResultV3(_)
            | SessionLogEntry::RuntimeContextSnapshotV2(_) => {}
        }
    }

    fn apply_control(&mut self, control: &ControlEntry) {
        match control {
            ControlEntry::ConversationInputPromoted(_) => self.clear_current(),
            ControlEntry::PlanDraftCreated(_) => self.clear_current(),
            ControlEntry::ConversationRouteDecisionRecorded(entry)
                if matches!(
                    entry.route,
                    sigil_kernel::ConversationRoute::Chat
                        | sigil_kernel::ConversationRoute::PlanReview
                ) =>
            {
                self.clear_current();
            }
            ControlEntry::PlanReviewAttempt(entry)
                if entry.status == sigil_kernel::PlanReviewAttemptStatus::Started =>
            {
                self.clear_current();
            }
            ControlEntry::TaskHandoffResolved(entry)
                if entry.decision == sigil_kernel::TaskHandoffDecision::Accepted =>
            {
                if let Some(task_id) = entry.task_id.as_ref() {
                    self.select_current(task_id.as_str());
                }
            }
            ControlEntry::TaskCreatedFromPlan(entry) if entry.stale_reason.is_none() => {
                self.select_current(entry.task_id.as_str());
            }
            ControlEntry::TaskContinuationSelected(entry) => {
                let matches_frozen_task =
                    self.tasks.get(entry.task_id.as_str()).is_some_and(|task| {
                        task.status == task_run_status_label(entry.task_status)
                            && task.plan_version == entry.plan_version
                            && task.plan_status.as_deref()
                                == entry.plan_status.map(task_plan_status_label)
                    });
                if matches_frozen_task {
                    self.select_current(entry.task_id.as_str());
                } else {
                    self.clear_current();
                }
            }
            ControlEntry::TaskGuidancePromoted(entry) => {
                let matches_accepted_plan =
                    self.tasks.get(entry.task_id.as_str()).is_some_and(|task| {
                        !matches!(task.status.as_str(), "completed" | "cancelled")
                            && task.plan_version == Some(entry.plan_version)
                            && task.plan_status.as_deref() == Some("accepted")
                    });
                if matches_accepted_plan {
                    self.select_current(entry.task_id.as_str());
                } else {
                    self.clear_current();
                }
            }
            ControlEntry::TaskRunCancellationScopeBound(entry) => {
                self.task_run_scopes.insert(
                    entry.task_id.as_str().to_owned(),
                    entry.run_scope_id.clone(),
                );
            }
            ControlEntry::TaskRunTargetSelected(entry) => {
                let matches_scope =
                    self.task_run_scopes.get(entry.task_id.as_str()) == Some(&entry.run_scope_id);
                let matches_frozen_task =
                    self.tasks.get(entry.task_id.as_str()).is_some_and(|task| {
                        task.status == task_run_status_label(entry.task_status)
                            && task.plan_version == entry.plan_version
                            && task.plan_status.as_deref()
                                == entry.plan_status.map(task_plan_status_label)
                    });
                if entry.validate_shape().is_ok() && matches_scope && matches_frozen_task {
                    self.select_current(entry.task_id.as_str());
                }
            }
            ControlEntry::TaskRun(entry)
                if entry.status == sigil_kernel::TaskRunStatus::Started
                    && !self.tasks.contains_key(entry.task_id.as_str()) =>
            {
                self.select_current(entry.task_id.as_str());
            }
            _ => {}
        }
        for event in self.events.project_control(control) {
            self.apply_event(event);
        }
    }

    fn current(&self) -> Option<ConversationTaskControlV1> {
        self.current_task_id
            .as_ref()
            .and_then(|task_id| self.tasks.get(task_id))
            .filter(|task| !matches!(task.status.as_str(), "completed" | "cancelled"))
            .cloned()
    }

    fn clear_current(&mut self) {
        self.current_task_id = None;
        self.focus_explicitly_selected = true;
    }

    fn select_current(&mut self, task_id: &str) {
        self.current_task_id = Some(task_id.to_owned());
        self.focus_explicitly_selected = true;
    }

    fn apply_event(&mut self, event: PublicRunEventKind) {
        match event {
            PublicRunEventKind::TaskPhaseChanged {
                task_id: Some(task_id),
                phase,
                status,
            } => {
                let is_final = matches!(status.as_str(), "completed" | "cancelled");
                {
                    let task = self.task(&task_id);
                    if matches!(task.status.as_str(), "completed" | "cancelled")
                        && task.status != status
                    {
                        return;
                    }
                    task.phase = phase;
                    task.can_continue = !is_final;
                    task.status = status;
                    if is_final {
                        task.steps.clear();
                        task.lanes.clear();
                    }
                }
            }
            PublicRunEventKind::TaskPlanUpdated {
                task_id,
                plan_version,
                status,
                steps,
            } => {
                let task = self.task(&task_id);
                if matches!(task.status.as_str(), "completed" | "cancelled") {
                    return;
                }
                if task
                    .plan_version
                    .is_some_and(|current| plan_version < current)
                {
                    return;
                }
                let previous_statuses = if task.plan_version == Some(plan_version) {
                    task.steps
                        .iter()
                        .filter_map(|step| {
                            step.status
                                .clone()
                                .map(|status| (step.step_id.clone(), status))
                        })
                        .collect::<BTreeMap<_, _>>()
                } else {
                    BTreeMap::new()
                };
                task.plan_version = Some(plan_version);
                task.plan_status = Some(status);
                task.steps_truncated = steps.len() > MAX_CONVERSATION_TASK_CONTROL_ITEMS
                    || steps.iter().any(|step| {
                        step.depends_on.len() > MAX_CONVERSATION_TASK_CONTROL_DETAIL_ITEMS
                            || step.title.len() > MAX_CONVERSATION_TASK_CONTROL_TITLE_BYTES
                    });
                task.steps = steps
                    .into_iter()
                    .take(MAX_CONVERSATION_TASK_CONTROL_ITEMS)
                    .map(|step| ConversationTaskPlanStepV1 {
                        status: previous_statuses.get(&step.step_id).cloned(),
                        step_id: step.step_id,
                        title: truncate_utf8(
                            &step.title,
                            MAX_CONVERSATION_TASK_CONTROL_TITLE_BYTES,
                        )
                        .0,
                        role: step.role,
                        depends_on: step
                            .depends_on
                            .into_iter()
                            .take(MAX_CONVERSATION_TASK_CONTROL_DETAIL_ITEMS)
                            .collect(),
                        mode: step.mode,
                        isolation: step.isolation,
                    })
                    .collect();
            }
            PublicRunEventKind::TaskBatchChanged {
                task_id,
                plan_version,
                active,
                completed,
                failed,
                ..
            } => {
                let task = self.task(&task_id);
                if matches!(task.status.as_str(), "completed" | "cancelled") {
                    return;
                }
                if task
                    .plan_version
                    .is_some_and(|current| plan_version < current)
                {
                    return;
                }
                task.plan_version = Some(plan_version);
                task.active_children = active;
                task.completed_children = completed;
                task.failed_children = failed;
            }
            PublicRunEventKind::TaskStepChanged {
                task_id,
                plan_version,
                step_id,
                status,
                ..
            } => {
                let task = self.task(&task_id);
                if matches!(task.status.as_str(), "completed" | "cancelled") {
                    return;
                }
                if task
                    .plan_version
                    .is_some_and(|current| plan_version < current)
                {
                    return;
                }
                task.plan_version = Some(plan_version);
                if let Some(step) = task.steps.iter_mut().find(|step| step.step_id == step_id) {
                    step.status = Some(status);
                } else if task.steps.len() < MAX_CONVERSATION_TASK_CONTROL_ITEMS {
                    task.steps.push(ConversationTaskPlanStepV1 {
                        title: step_id.clone(),
                        step_id,
                        role: "unknown".to_owned(),
                        depends_on: Vec::new(),
                        mode: "unknown".to_owned(),
                        isolation: "unknown".to_owned(),
                        status: Some(status),
                    });
                } else {
                    task.steps_truncated = true;
                }
            }
            PublicRunEventKind::IntegrationLaneChanged {
                task_id,
                plan_version,
                plan_id,
                lane_id,
                status,
                conflicts,
            } => {
                let task = self.task(&task_id);
                if matches!(task.status.as_str(), "completed" | "cancelled") {
                    return;
                }
                if task
                    .plan_version
                    .is_some_and(|current| plan_version < current)
                {
                    return;
                }
                task.plan_version = Some(plan_version);
                let conflicts_truncated =
                    conflicts.len() > MAX_CONVERSATION_TASK_CONTROL_DETAIL_ITEMS;
                let lane = ConversationTaskLaneV1 {
                    lane_id: lane_id.clone(),
                    plan_id: Some(plan_id),
                    status,
                    conflicts: conflicts
                        .into_iter()
                        .take(MAX_CONVERSATION_TASK_CONTROL_DETAIL_ITEMS)
                        .collect(),
                };
                task.lanes_truncated |= conflicts_truncated;
                if let Some(existing) = task.lanes.iter_mut().find(|lane| lane.lane_id == lane_id) {
                    *existing = lane;
                } else if task.lanes.len() < MAX_CONVERSATION_TASK_CONTROL_ITEMS {
                    task.lanes.push(lane);
                } else {
                    task.lanes_truncated = true;
                }
            }
            _ => {}
        }
    }

    fn task(&mut self, task_id: &str) -> &mut ConversationTaskControlV1 {
        if !self.focus_explicitly_selected {
            self.current_task_id = Some(task_id.to_owned());
        }
        self.tasks
            .entry(task_id.to_owned())
            .or_insert_with(|| ConversationTaskControlV1 {
                schema_version: 1,
                task_id: task_id.to_owned(),
                phase: PublicTaskPhase::Planning,
                status: "started".to_owned(),
                plan_version: None,
                plan_status: None,
                steps: Vec::new(),
                steps_truncated: false,
                active_children: 0,
                completed_children: 0,
                failed_children: 0,
                lanes: Vec::new(),
                lanes_truncated: false,
                can_continue: true,
            })
    }
}

fn task_run_status_label(status: sigil_kernel::TaskRunStatus) -> &'static str {
    match status {
        sigil_kernel::TaskRunStatus::Started => "started",
        sigil_kernel::TaskRunStatus::Running => "running",
        sigil_kernel::TaskRunStatus::Paused => "paused",
        sigil_kernel::TaskRunStatus::Completed => "completed",
        sigil_kernel::TaskRunStatus::Failed => "failed",
        sigil_kernel::TaskRunStatus::Cancelled => "cancelled",
        sigil_kernel::TaskRunStatus::Interrupted => "interrupted",
    }
}

fn task_plan_status_label(status: sigil_kernel::TaskPlanStatus) -> &'static str {
    match status {
        sigil_kernel::TaskPlanStatus::Proposed => "proposed",
        sigil_kernel::TaskPlanStatus::Accepted => "accepted",
        sigil_kernel::TaskPlanStatus::Superseded => "superseded",
        sigil_kernel::TaskPlanStatus::Rejected => "rejected",
    }
}

/// Derives an opaque renderer identity for one live semantic slot.
///
/// # Errors
///
/// Returns an error when the durable scope, run id, or slot identity is empty or unbounded.
pub fn conversation_live_provisional_id(
    session_scope: &str,
    run_id: &str,
    slot: &ConversationLiveProvisionalSlotV1,
) -> Result<String> {
    validate_provisional_identity("session scope", session_scope)?;
    validate_provisional_identity("run id", run_id)?;
    let (slot_kind, slot_identity) = match slot {
        ConversationLiveProvisionalSlotV1::User => ("user", None),
        ConversationLiveProvisionalSlotV1::AssistantMessage { message_id } => {
            validate_provisional_identity("assistant message id", message_id)?;
            ("assistant_message", Some(message_id.as_str()))
        }
        ConversationLiveProvisionalSlotV1::Tool { call_id } => {
            validate_provisional_identity("tool call id", call_id)?;
            ("tool", Some(call_id.as_str()))
        }
        ConversationLiveProvisionalSlotV1::Approval { call_id } => {
            validate_provisional_identity("approval call id", call_id)?;
            ("approval", Some(call_id.as_str()))
        }
        ConversationLiveProvisionalSlotV1::Terminal => ("terminal", None),
    };
    let mut digest = Sha256::new();
    digest.update(b"sigil-conversation-live-v1\0");
    digest.update(session_scope.as_bytes());
    digest.update(b"\0");
    digest.update(run_id.as_bytes());
    digest.update(b"\0");
    digest.update(slot_kind.as_bytes());
    if let Some(identity) = slot_identity {
        digest.update(b"\0");
        digest.update(identity.as_bytes());
    }
    Ok(format!("live-v1:{:x}", digest.finalize()))
}

/// Reads a validated JSONL session and projects one canonical display page.
///
/// # Errors
///
/// Fails closed on malformed/tampered records, unknown recovery-critical events, scope or cursor
/// mismatch, invalid run lifecycle ordering, and invalid page bounds.
pub fn conversation_display_page(
    session_path: &Path,
    expected_scope: &str,
    cursor: Option<&str>,
    limit: usize,
    current_workspace_snapshot_id: Option<&str>,
) -> std::result::Result<ConversationDisplayPageV1, ConversationDisplayProjectionError> {
    let records = JsonlSessionStore::read_event_records(session_path).with_context(|| {
        format!(
            "failed to read conversation session {}",
            session_path.display()
        )
    })?;
    let mut page = conversation_display_page_from_records(
        &records,
        expected_scope,
        cursor,
        limit,
        current_workspace_snapshot_id,
    )?;
    reconcile_physical_artifact_availability(
        &mut page,
        &ToolArtifactStore::for_session_path(session_path),
    );
    Ok(page)
}

fn reconcile_physical_artifact_availability(
    page: &mut ConversationDisplayPageV1,
    store: &ToolArtifactStore,
) {
    for item in &mut page.items {
        let ConversationDisplayContentV1::Tool {
            artifact_ref: Some(artifact_id),
            artifact_availability,
            ..
        } = &mut item.content
        else {
            continue;
        };
        let reference = ToolArtifactRefV1 {
            artifact_id: artifact_id.clone(),
        };
        *artifact_availability = Some(
            match store.resolve(&reference) {
                Ok(descriptor) => tool_artifact_availability_label(store.availability(&descriptor)),
                Err(_) => "missing",
            }
            .to_owned(),
        );
    }
}

/// Projects one page from already-loaded durable stream records.
///
/// This entry point exists for adapters that already own a validated session snapshot and for
/// deterministic contract tests. It preserves the same fixed-frontier and fail-closed behavior as
/// [`conversation_display_page`].
///
/// # Errors
///
/// Returns an error for invalid bounds, scope/order/checksum violations, invalid cursor state, or
/// malformed critical durable records.
pub fn conversation_display_page_from_records(
    records: &[SessionStreamRecord],
    expected_scope: &str,
    cursor: Option<&str>,
    limit: usize,
    current_workspace_snapshot_id: Option<&str>,
) -> std::result::Result<ConversationDisplayPageV1, ConversationDisplayProjectionError> {
    validate_page_request(expected_scope, limit)?;

    let decoded_cursor = cursor
        .map(decode_cursor)
        .transpose()
        .map_err(ConversationDisplayProjectionError::invalid_cursor)?;
    if let Some(cursor) = decoded_cursor.as_ref() {
        validate_cursor_request(cursor, expected_scope)
            .map_err(ConversationDisplayProjectionError::invalid_cursor)?;
    }
    validate_stream(records, expected_scope)?;
    let frontier = fixed_frontier(records, expected_scope, decoded_cursor.as_ref())
        .map_err(ConversationDisplayProjectionError::stale_cursor)?;
    let before_order = decoded_cursor.as_ref().map(|cursor| cursor.before_order);
    let capacity = limit.saturating_add(1);
    let mut recent = VecDeque::with_capacity(capacity);
    let mut active_run: Option<ActiveRunProjection> = None;
    let mut tools = HashMap::<String, ToolProjection>::new();
    let mut approval_items = HashMap::<String, String>::new();
    let mut run_skills = HashMap::<String, ConversationDisplaySkillReferenceV1>::new();
    let mut terminal_frontier = None;
    let mut task_control = ConversationTaskControlProjection::default();
    let mut plan_review = PlanReviewDisplayProjection::default();
    let mut user_input = UserInputProjectionV1::default();
    let mut agent_user_input = sigil_kernel::AgentUserInputRouteProjectionV1::default();
    let mut total_items = 0_u64;
    let mut eligible_items = 0_u64;
    let mut cursor_boundary_found = decoded_cursor.is_none();

    for record in records
        .iter()
        .take_while(|record| record.stream_sequence() <= frontier.sequence)
    {
        plan_review.apply_record(record);
        if let Some(SessionLogEntry::Control(control)) = record.session_log_entry()?
            && let Some(entry) = UserInputLifecycleEntryV1::from_control(&control)
        {
            user_input.apply(entry)?;
        }
        if let Some(SessionLogEntry::Control(ControlEntry::AgentUserInputRoute(route))) =
            record.session_log_entry()?
        {
            agent_user_input.apply(route)?;
        }
        let mut projected = project_record(
            record,
            expected_scope,
            &mut active_run,
            &mut tools,
            &mut approval_items,
            &mut run_skills,
            &mut terminal_frontier,
            &mut task_control,
        )?;
        projected.sort_by_key(|item| item.display_order);
        for item in projected {
            if before_order == Some(item.display_order) {
                cursor_boundary_found = true;
            }
            total_items = total_items
                .checked_add(1)
                .ok_or_else(|| anyhow!("conversation display item count overflow"))?;
            if before_order.is_some_and(|before| item.display_order >= before) {
                continue;
            }
            eligible_items = eligible_items
                .checked_add(1)
                .ok_or_else(|| anyhow!("conversation display eligible count overflow"))?;
            recent.push_back(item);
            if recent.len() > capacity {
                recent.pop_front();
            }
        }
    }
    if !cursor_boundary_found {
        return Err(ConversationDisplayProjectionError::stale_cursor(anyhow!(
            "conversation display cursor boundary is not a projected item"
        )));
    }

    let mut selected_reversed = Vec::new();
    let mut selected_bytes = 0_usize;
    for item in recent.iter().rev() {
        if selected_reversed.len() == limit {
            break;
        }
        let item_bytes = serde_json::to_vec(item)
            .context("failed to measure canonical conversation display item")?
            .len();
        if !selected_reversed.is_empty()
            && selected_bytes.saturating_add(item_bytes) > MAX_CONVERSATION_DISPLAY_PAGE_BYTES
        {
            break;
        }
        selected_bytes = selected_bytes.saturating_add(item_bytes);
        selected_reversed.push(item.clone());
    }
    selected_reversed.reverse();
    let items = selected_reversed;
    let has_more = eligible_items > u64::try_from(items.len()).unwrap_or(u64::MAX);
    let next_cursor = if has_more {
        let oldest = items
            .first()
            .ok_or_else(|| anyhow!("bounded display page could not retain one item"))?;
        Some(encode_cursor(&ConversationDisplayCursorV1 {
            schema_version: CONVERSATION_DISPLAY_SCHEMA_VERSION,
            session_scope_sha256: scope_sha256(expected_scope),
            through_session_stream_sequence: frontier.sequence,
            frontier_binding_sha256: frontier_binding_sha256(
                expected_scope,
                records,
                frontier.sequence,
                oldest.display_order,
            ),
            before_order: oldest.display_order,
        })?)
    } else {
        None
    };

    let projected_user_inputs = stable_pending_user_inputs(
        user_input
            .pending()
            .map(sigil_kernel::UserInputRequestStateV1::public_view)
            .chain(plan_review.pending_user_input())
            .chain(
                agent_user_input
                    .unresolved()
                    .map(|route| route.request.clone()),
            ),
    );
    let projected_user_input = projected_user_inputs.first().cloned();
    Ok(ConversationDisplayPageV1 {
        schema_version: CONVERSATION_DISPLAY_SCHEMA_VERSION,
        session_scope_id: expected_scope.to_owned(),
        through_session_stream_sequence: frontier.sequence,
        terminal_frontier,
        total_items,
        items,
        next_cursor,
        has_more,
        task_control: task_control.current(),
        plan_review: plan_review.into_public(current_workspace_snapshot_id),
        user_inputs: projected_user_inputs,
        user_input: projected_user_input,
    })
}

fn validate_page_request(expected_scope: &str, limit: usize) -> Result<()> {
    if expected_scope.is_empty() || expected_scope.len() > MAX_CONVERSATION_DISPLAY_IDENTITY_BYTES {
        bail!("conversation display requires a bounded non-empty session scope");
    }
    if limit == 0 || limit > MAX_CONVERSATION_DISPLAY_PAGE_SIZE {
        bail!(
            "conversation display page size must be between 1 and {MAX_CONVERSATION_DISPLAY_PAGE_SIZE}"
        );
    }
    Ok(())
}

fn validate_provisional_identity(label: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_CONVERSATION_DISPLAY_IDENTITY_BYTES {
        bail!("conversation live provisional {label} must be bounded and non-empty");
    }
    Ok(())
}

fn validate_stream(records: &[SessionStreamRecord], expected_scope: &str) -> Result<()> {
    let mut previous = 0_u64;
    for record in records {
        if record.session_id() != expected_scope {
            bail!("conversation display session scope mismatch");
        }
        let expected_sequence = previous
            .checked_add(1)
            .ok_or_else(|| anyhow!("conversation display stream sequence overflow"))?;
        if record.stream_sequence() != expected_sequence {
            bail!("conversation display stream order is invalid");
        }
        record
            .stored_event()
            .verify_record_checksum()
            .context("conversation display stream checksum verification failed")?;
        previous = record.stream_sequence();
    }
    Ok(())
}

fn fixed_frontier(
    records: &[SessionStreamRecord],
    expected_scope: &str,
    cursor: Option<&ConversationDisplayCursorV1>,
) -> Result<FixedFrontier> {
    let Some(cursor) = cursor else {
        return Ok(records.last().map_or_else(
            || FixedFrontier { sequence: 0 },
            |record| FixedFrontier {
                sequence: record.stream_sequence(),
            },
        ));
    };
    let frontier = records
        .iter()
        .find(|record| record.stream_sequence() == cursor.through_session_stream_sequence)
        .ok_or_else(|| anyhow!("conversation display cursor frontier is unavailable"))?;
    let expected_binding = frontier_binding_sha256(
        expected_scope,
        records,
        frontier.stream_sequence(),
        cursor.before_order,
    );
    if expected_binding != cursor.frontier_binding_sha256 {
        bail!("conversation display cursor frontier no longer matches durable history");
    }
    Ok(FixedFrontier {
        sequence: cursor.through_session_stream_sequence,
    })
}

fn validate_cursor_request(
    cursor: &ConversationDisplayCursorV1,
    expected_scope: &str,
) -> Result<()> {
    if cursor.schema_version != CONVERSATION_DISPLAY_SCHEMA_VERSION {
        bail!("unsupported conversation display cursor schema");
    }
    if cursor.session_scope_sha256 != scope_sha256(expected_scope) {
        bail!("conversation display cursor belongs to another session scope");
    }
    if cursor.before_order.session_stream_sequence == 0
        || cursor.before_order.session_stream_sequence > cursor.through_session_stream_sequence
    {
        bail!("conversation display cursor order is outside its fixed frontier");
    }
    Ok(())
}

fn project_record(
    record: &SessionStreamRecord,
    expected_scope: &str,
    active_run: &mut Option<ActiveRunProjection>,
    tools: &mut HashMap<String, ToolProjection>,
    approval_items: &mut HashMap<String, String>,
    run_skills: &mut HashMap<String, ConversationDisplaySkillReferenceV1>,
    terminal_frontier: &mut Option<ConversationTerminalFrontierV1>,
    task_control: &mut ConversationTaskControlProjection,
) -> Result<Vec<ConversationDisplayItemV1>> {
    if let Some(lifecycle) = conversation_run_lifecycle_record_from_stream(record)? {
        return project_lifecycle(
            record,
            expected_scope,
            lifecycle,
            active_run,
            tools,
            approval_items,
            terminal_frontier,
        );
    }

    if let Some(entry) = record.session_log_entry()? {
        task_control.apply_entry(&entry);
        return project_session_entry(
            record,
            expected_scope,
            entry,
            active_run,
            tools,
            approval_items,
            run_skills,
        );
    }

    let Some(event_type) = record.stored_event().event_kind() else {
        // `session_log_entry` already performed the fail-closed critical decode.
        return Ok(Vec::new());
    };
    if event_type == DurableEventType::CompactionAppliedV2 {
        let _: CompactionAppliedV2 = serde_json::from_value(record.stored_event().payload.clone())
            .context("failed to decode context compaction display source")?;
        return Ok(vec![new_notice_item(
            expected_scope,
            record,
            active_run_id(active_run),
            "Context compaction applied. Durable task memory now carries the folded history.",
        )]);
    }
    if event_type == DurableEventType::ConversationForked {
        let fork: ConversationForked =
            serde_json::from_value(record.stored_event().payload.clone())
                .context("failed to decode conversation fork display source")?;
        return Ok(vec![new_notice_item(
            expected_scope,
            record,
            active_run_id(active_run),
            &format!(
                "Conversation fork created from turn {}. The original conversation and workspace files were not changed.",
                fork.source_turn_index
            ),
        )]);
    }
    if event_type == DurableEventType::CheckpointRestored {
        let _: CheckpointRestored =
            serde_json::from_value(record.stored_event().payload.clone())
                .context("failed to decode checkpoint restored display source")?;
        return Ok(vec![new_item(
            expected_scope,
            record,
            0,
            ConversationDisplayItemKindV1::Checkpoint,
            ConversationDisplaySourceV1::DurableRunEvent,
            active_run_id(active_run),
            ConversationDisplayStatusV1::Completed,
            ConversationDisplayContentV1::Checkpoint {
                outcome: ConversationDisplayCheckpointOutcomeV1::Restored,
                checkpoint_id: None,
                conflict_reason: None,
            },
        )]);
    }
    if let Some(typed) = record.typed_domain_event_record()?
        && let TypedDomainEvent::CheckpointRestoreConflict(conflict) = typed.event
    {
        return Ok(vec![new_item(
            expected_scope,
            record,
            0,
            ConversationDisplayItemKindV1::Checkpoint,
            ConversationDisplaySourceV1::DurableRunEvent,
            active_run_id(active_run),
            ConversationDisplayStatusV1::Failed,
            ConversationDisplayContentV1::Checkpoint {
                outcome: ConversationDisplayCheckpointOutcomeV1::Conflict,
                checkpoint_id: Some(bound_identity(&conflict.checkpoint_id)),
                conflict_reason: Some(map_checkpoint_conflict_reason(conflict.reason)),
            },
        )]);
    }
    Ok(Vec::new())
}

fn new_notice_item(
    expected_scope: &str,
    record: &SessionStreamRecord,
    run_id: Option<String>,
    text: &str,
) -> ConversationDisplayItemV1 {
    let text = project_text(text);
    new_item(
        expected_scope,
        record,
        0,
        ConversationDisplayItemKindV1::Notice,
        ConversationDisplaySourceV1::DurableRunEvent,
        run_id,
        ConversationDisplayStatusV1::Completed,
        ConversationDisplayContentV1::Notice {
            text: text.text,
            truncated: text.truncated,
            original_content_bytes: text.original_bytes,
        },
    )
}

fn project_lifecycle(
    record: &SessionStreamRecord,
    expected_scope: &str,
    lifecycle: ConversationRunLifecycleRecordV1,
    active_run: &mut Option<ActiveRunProjection>,
    tools: &mut HashMap<String, ToolProjection>,
    approval_items: &mut HashMap<String, String>,
    terminal_frontier: &mut Option<ConversationTerminalFrontierV1>,
) -> Result<Vec<ConversationDisplayItemV1>> {
    match lifecycle {
        ConversationRunLifecycleRecordV1::ConversationRunStartedV1(started) => {
            if active_run.is_some() {
                bail!("conversation display encountered overlapping durable runs");
            }
            tools.clear();
            approval_items.clear();
            *active_run = Some(ActiveRunProjection {
                run_id: started.run_id().to_owned(),
                final_message_id: None,
                user_provisional_reconciled: false,
            });
            Ok(Vec::new())
        }
        ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(finalized) => {
            let Some(active) = active_run.as_ref() else {
                bail!("conversation display terminal has no matching durable start");
            };
            if active.run_id != finalized.run_id() {
                bail!("conversation display terminal belongs to another active run");
            }
            match finalized.status() {
                ConversationRunTerminalStatusV1::Succeeded => {
                    let durable_final = active.final_message_id.as_deref().ok_or_else(|| {
                        anyhow!("succeeded conversation run has no durable final assistant")
                    })?;
                    if finalized.final_message_id() != Some(durable_final) {
                        bail!(
                            "succeeded conversation run terminal does not match its durable final assistant"
                        );
                    }
                }
                _ if finalized.final_message_id().is_some() => {
                    bail!("non-succeeded conversation run must not bind a final message id");
                }
                _ => {}
            }
            let status = map_terminal_status(finalized.status())?;
            let run_id = finalized.run_id().to_owned();
            *terminal_frontier = Some(ConversationTerminalFrontierV1 {
                run_id: run_id.clone(),
                session_stream_sequence: record.stream_sequence(),
                status,
            });
            *active_run = None;
            tools.clear();
            approval_items.clear();
            let mut item = new_item(
                expected_scope,
                record,
                0,
                ConversationDisplayItemKindV1::Terminal,
                ConversationDisplaySourceV1::DurableRunEvent,
                Some(run_id.clone()),
                status,
                ConversationDisplayContentV1::Terminal {
                    final_message_id: finalized.final_message_id().map(ToOwned::to_owned),
                    safe_summary: finalized.safe_summary().map(ToOwned::to_owned),
                    summary_truncated: finalized.summary_truncated(),
                },
            );
            item.reconciles = Some(vec![conversation_live_provisional_id(
                expected_scope,
                &run_id,
                &ConversationLiveProvisionalSlotV1::Terminal,
            )?]);
            Ok(vec![item])
        }
    }
}

fn project_session_entry(
    record: &SessionStreamRecord,
    expected_scope: &str,
    entry: SessionLogEntry,
    active_run: &mut Option<ActiveRunProjection>,
    tools: &mut HashMap<String, ToolProjection>,
    approval_items: &mut HashMap<String, String>,
    run_skills: &mut HashMap<String, ConversationDisplaySkillReferenceV1>,
) -> Result<Vec<ConversationDisplayItemV1>> {
    match entry {
        SessionLogEntry::User(message) => {
            let (run_id, reconcile_live_user) = match active_run.as_mut() {
                Some(active) => {
                    let reconcile_live_user = !active.user_provisional_reconciled;
                    active.user_provisional_reconciled = true;
                    (Some(active.run_id.clone()), reconcile_live_user)
                }
                None => (None, false),
            };
            let skill = run_id
                .as_ref()
                .and_then(|run_id| run_skills.get(run_id))
                .cloned();
            project_durable_user_message(
                record,
                expected_scope,
                message,
                run_id,
                skill,
                reconcile_live_user,
            )
        }
        SessionLogEntry::Assistant(message) => {
            if message.role != MessageRole::Assistant {
                bail!("conversation display assistant entry has a non-assistant role");
            }
            if message.assistant_kind == Some(AssistantMessageKind::FinalAnswer)
                && let Some(active) = active_run.as_mut()
                && active
                    .final_message_id
                    .replace(message.id.clone())
                    .is_some()
            {
                bail!("conversation run contains more than one durable final assistant");
            }
            let run_id = active_run_id(active_run);
            let assistant_provisional = run_id
                .as_deref()
                .map(|run_id| {
                    conversation_live_provisional_id(
                        expected_scope,
                        run_id,
                        &ConversationLiveProvisionalSlotV1::AssistantMessage {
                            message_id: message.id.clone(),
                        },
                    )
                })
                .transpose()?;
            let mut items = Vec::new();
            let mut subindex = 0_u32;
            if let Some(content) = project_optional_text(message.content.as_deref()) {
                if message.assistant_kind == Some(AssistantMessageKind::ReasoningTrace) {
                    let mut item = new_item(
                        expected_scope,
                        record,
                        subindex,
                        ConversationDisplayItemKindV1::Reasoning,
                        ConversationDisplaySourceV1::DurableTranscript,
                        run_id.clone(),
                        ConversationDisplayStatusV1::Recorded,
                        ConversationDisplayContentV1::Reasoning {
                            text: content.text,
                            truncated: content.truncated,
                            original_content_bytes: content.original_bytes,
                        },
                    );
                    item.reconciles = assistant_provisional.clone().map(|id| vec![id]);
                    items.push(item);
                } else {
                    let mut item = new_message_item(
                        expected_scope,
                        record,
                        subindex,
                        run_id.clone(),
                        ConversationDisplayMessageRoleV1::Assistant,
                        Some(content),
                        None,
                        map_assistant_phase(message.assistant_kind),
                        message.image_attachments.len(),
                    );
                    item.reconciles = assistant_provisional.clone().map(|id| vec![id]);
                    items.push(item);
                }
                subindex = subindex
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("conversation display subindex overflow"))?;
            }
            for call in message.tool_calls {
                let tool_name_key = call.id.clone();
                let call_id = bound_identity(&call.id);
                let tool_name = bound_identity(&call.name);
                let mut item = new_item(
                    expected_scope,
                    record,
                    subindex,
                    ConversationDisplayItemKindV1::Tool,
                    ConversationDisplaySourceV1::DurableTranscript,
                    run_id.clone(),
                    ConversationDisplayStatusV1::Requested,
                    ConversationDisplayContentV1::Tool {
                        call_id: Some(call_id),
                        tool_name: Some(tool_name.clone()),
                        output: None,
                        truncated: false,
                        original_content_bytes: 0,
                        artifact_ref: None,
                        artifact_availability: None,
                        observed_bytes: None,
                        persisted_bytes: None,
                        has_more: false,
                        preview_truncated: false,
                        truncation_reason: None,
                        capture_completeness: None,
                    },
                );
                if let Some(run_id) = run_id.as_deref() {
                    item.reconciles = Some(vec![conversation_live_provisional_id(
                        expected_scope,
                        run_id,
                        &ConversationLiveProvisionalSlotV1::Tool {
                            call_id: call.id.clone(),
                        },
                    )?]);
                }
                tools.insert(
                    tool_name_key,
                    ToolProjection {
                        name: tool_name,
                        requested_display_id: item.display_id.clone(),
                    },
                );
                items.push(item);
                subindex = subindex
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("conversation display subindex overflow"))?;
            }
            Ok(items)
        }
        SessionLogEntry::ToolResultV3(result) => {
            let tool = tools.get(&result.call_id).cloned();
            let display = result.display_view();
            let (artifact_ref, artifact_availability) = match &result.artifact {
                sigil_kernel::ToolArtifactBindingV1::Published { descriptor } => (
                    Some(descriptor.artifact_ref.artifact_id.clone()),
                    Some(
                        if descriptor.retrieval_available() {
                            "available"
                        } else {
                            "policy_revoked"
                        }
                        .to_owned(),
                    ),
                ),
                sigil_kernel::ToolArtifactBindingV1::Unavailable { unavailable } => (
                    None,
                    Some(tool_artifact_availability_label(unavailable.availability).to_owned()),
                ),
            };
            let run_id = active_run_id(active_run);
            let mut item = new_item(
                expected_scope,
                record,
                0,
                ConversationDisplayItemKindV1::Tool,
                ConversationDisplaySourceV1::DurableTranscript,
                run_id.clone(),
                ConversationDisplayStatusV1::Completed,
                ConversationDisplayContentV1::Tool {
                    call_id: Some(bound_identity(&result.call_id)),
                    tool_name: Some(tool.as_ref().map_or_else(
                        || bound_identity(&result.tool_name),
                        |tool| tool.name.clone(),
                    )),
                    output: Some(display.preview),
                    truncated: display.has_more,
                    original_content_bytes: display.observed_bytes as usize,
                    artifact_ref,
                    artifact_availability,
                    observed_bytes: Some(display.observed_bytes),
                    persisted_bytes: Some(display.persisted_bytes),
                    has_more: display.has_more,
                    preview_truncated: display.preview_truncated,
                    truncation_reason: display
                        .truncation_reason
                        .map(|reason| reason.as_str().to_owned()),
                    capture_completeness: display.capture_completeness.map(|completeness| {
                        format!(
                            "source={},policy={},storage={}",
                            completeness.source.as_str(),
                            completeness.policy.as_str(),
                            completeness.storage.as_str()
                        )
                    }),
                },
            );
            let mut reconciles = tool
                .as_ref()
                .map(|tool| vec![tool.requested_display_id.clone()])
                .unwrap_or_default();
            if let Some(run_id) = run_id.as_deref() {
                reconciles.push(conversation_live_provisional_id(
                    expected_scope,
                    run_id,
                    &ConversationLiveProvisionalSlotV1::Tool {
                        call_id: result.call_id,
                    },
                )?);
            }
            if !reconciles.is_empty() {
                item.reconciles = Some(reconciles);
            }
            Ok(vec![item])
        }
        SessionLogEntry::RuntimeContextSnapshotV2(_) => Ok(Vec::new()),
        SessionLogEntry::Control(control) => project_control(
            record,
            expected_scope,
            control,
            active_run,
            approval_items,
            run_skills,
        ),
    }
}

fn tool_artifact_availability_label(
    availability: sigil_kernel::ToolArtifactAvailability,
) -> &'static str {
    match availability {
        sigil_kernel::ToolArtifactAvailability::Available => "available",
        sigil_kernel::ToolArtifactAvailability::Expired => "expired",
        sigil_kernel::ToolArtifactAvailability::Missing => "missing",
        sigil_kernel::ToolArtifactAvailability::HashMismatch => "hash_mismatch",
        sigil_kernel::ToolArtifactAvailability::PolicyRevoked => "policy_revoked",
        sigil_kernel::ToolArtifactAvailability::Unavailable => "unavailable",
    }
}

fn project_control(
    record: &SessionStreamRecord,
    expected_scope: &str,
    control: ControlEntry,
    active_run: &mut Option<ActiveRunProjection>,
    approval_items: &mut HashMap<String, String>,
    run_skills: &mut HashMap<String, ConversationDisplaySkillReferenceV1>,
) -> Result<Vec<ConversationDisplayItemV1>> {
    match control {
        ControlEntry::SkillLoaded(entry) => {
            if let Some(run_id) = entry.run_id {
                let name = entry
                    .display_name
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| entry.skill_id.clone());
                run_skills.insert(
                    run_id,
                    ConversationDisplaySkillReferenceV1 {
                        id: bound_identity(&entry.skill_id),
                        name: bound_identity(&name),
                    },
                );
            }
            Ok(Vec::new())
        }
        ControlEntry::ConversationInputPromoted(promotion) => {
            project_promoted_user_message(record, expected_scope, promotion, active_run)
        }
        ControlEntry::Note { kind, data } if kind == "reasoning_trace" => {
            let Some(text) = data.get("text").and_then(serde_json::Value::as_str) else {
                bail!("reasoning trace note is missing text");
            };
            let text = project_text(text);
            Ok(vec![new_item(
                expected_scope,
                record,
                0,
                ConversationDisplayItemKindV1::Reasoning,
                ConversationDisplaySourceV1::DurableTranscript,
                active_run_id(active_run),
                ConversationDisplayStatusV1::Recorded,
                ConversationDisplayContentV1::Reasoning {
                    text: text.text,
                    truncated: text.truncated,
                    original_content_bytes: text.original_bytes,
                },
            )])
        }
        ControlEntry::ToolApproval(approval) => {
            let raw_call_id = approval.call_id.clone();
            let (status, decision) = match approval.action {
                ToolApprovalAuditAction::Requested => {
                    (ConversationDisplayStatusV1::WaitingForApproval, None)
                }
                ToolApprovalAuditAction::DecisionAccepted | ToolApprovalAuditAction::Resolved => {
                    let decision = approval.user_decision.map(map_approval_decision);
                    let status = match decision {
                        Some(ConversationDisplayApprovalDecisionV1::Denied) => {
                            ConversationDisplayStatusV1::Denied
                        }
                        Some(_) => ConversationDisplayStatusV1::Approved,
                        None => ConversationDisplayStatusV1::Completed,
                    };
                    (status, decision)
                }
                ToolApprovalAuditAction::PreviewFailed => {
                    (ConversationDisplayStatusV1::Failed, None)
                }
            };
            let run_id = active_run_id(active_run);
            let mut item = new_item(
                expected_scope,
                record,
                0,
                ConversationDisplayItemKindV1::Approval,
                ConversationDisplaySourceV1::DurableRunEvent,
                run_id.clone(),
                status,
                ConversationDisplayContentV1::Approval {
                    call_id: bound_identity(&approval.call_id),
                    tool_name: bound_identity(&approval.tool_name),
                    decision,
                },
            );
            let mut reconciles = Vec::new();
            if approval.action != ToolApprovalAuditAction::Requested
                && let Some(requested) = approval_items.get(&raw_call_id)
            {
                reconciles.push(requested.clone());
            }
            if let Some(run_id) = run_id.as_deref() {
                reconciles.push(conversation_live_provisional_id(
                    expected_scope,
                    run_id,
                    &ConversationLiveProvisionalSlotV1::Approval {
                        call_id: raw_call_id.clone(),
                    },
                )?);
            }
            if !reconciles.is_empty() {
                item.reconciles = Some(reconciles);
            }
            // Approval V2 records both the accepted command receipt and the kernel-owned
            // terminal resolution. Keep a single reconciliation chain for every durable phase;
            // otherwise DecisionAccepted and Resolved would both claim the original request (and
            // live slot) as independent successors, which the Desktop continuity contract must
            // reject as an ambiguous branch.
            approval_items.insert(raw_call_id, item.display_id.clone());
            Ok(vec![item])
        }
        _ => Ok(Vec::new()),
    }
}

fn project_promoted_user_message(
    record: &SessionStreamRecord,
    expected_scope: &str,
    promotion: ConversationInputPromotedEntry,
    active_run: &mut Option<ActiveRunProjection>,
) -> Result<Vec<ConversationDisplayItemV1>> {
    promotion.validate_for_session(expected_scope)?;
    if let Some(active) = active_run.as_ref()
        && active.run_id != promotion.dispatch_run_id
    {
        bail!("conversation input promotion overlaps another durable run");
    }
    let reconcile_live_user = match active_run.as_mut() {
        Some(active) => {
            let reconcile_live_user = !active.user_provisional_reconciled;
            active.user_provisional_reconciled = true;
            reconcile_live_user
        }
        None => true,
    };
    project_durable_user_message(
        record,
        expected_scope,
        promotion.durable_user_message,
        Some(promotion.dispatch_run_id),
        None,
        reconcile_live_user,
    )
}

fn project_durable_user_message(
    record: &SessionStreamRecord,
    expected_scope: &str,
    message: ModelMessage,
    run_id: Option<String>,
    skill: Option<ConversationDisplaySkillReferenceV1>,
    reconcile_live_user: bool,
) -> Result<Vec<ConversationDisplayItemV1>> {
    if message.role != MessageRole::User {
        bail!("conversation display user entry has a non-user role");
    }
    let content = project_optional_text(message.content.as_deref());
    if content.is_none() && message.image_attachments.is_empty() {
        return Ok(Vec::new());
    }
    let mut item = new_message_item(
        expected_scope,
        record,
        0,
        run_id.clone(),
        ConversationDisplayMessageRoleV1::User,
        content,
        skill,
        None,
        message.image_attachments.len(),
    );
    if reconcile_live_user && let Some(run_id) = run_id {
        item.reconciles = Some(vec![conversation_live_provisional_id(
            expected_scope,
            &run_id,
            &ConversationLiveProvisionalSlotV1::User,
        )?]);
    }
    Ok(vec![item])
}

fn active_run_id(active_run: &Option<ActiveRunProjection>) -> Option<String> {
    active_run.as_ref().map(|active| active.run_id.clone())
}

#[derive(Debug)]
struct ProjectedText {
    text: String,
    truncated: bool,
    original_bytes: usize,
}

fn project_optional_text(value: Option<&str>) -> Option<ProjectedText> {
    value.filter(|value| !value.is_empty()).map(project_text)
}

fn project_text(value: &str) -> ProjectedText {
    let original_bytes = value.len();
    let safe = safe_persistence_text(value);
    let (text, truncated) = truncate_utf8(&safe, MAX_CONVERSATION_DISPLAY_CONTENT_BYTES);
    ProjectedText {
        text,
        truncated,
        original_bytes,
    }
}

fn new_message_item(
    expected_scope: &str,
    record: &SessionStreamRecord,
    subindex: u32,
    run_id: Option<String>,
    role: ConversationDisplayMessageRoleV1,
    text: Option<ProjectedText>,
    skill: Option<ConversationDisplaySkillReferenceV1>,
    assistant_phase: Option<ConversationDisplayAssistantPhaseV1>,
    image_attachment_count: usize,
) -> ConversationDisplayItemV1 {
    let kind = match role {
        ConversationDisplayMessageRoleV1::User => ConversationDisplayItemKindV1::UserMessage,
        ConversationDisplayMessageRoleV1::Assistant => {
            ConversationDisplayItemKindV1::AssistantMessage
        }
    };
    new_item(
        expected_scope,
        record,
        subindex,
        kind,
        ConversationDisplaySourceV1::DurableTranscript,
        run_id,
        ConversationDisplayStatusV1::Recorded,
        ConversationDisplayContentV1::Message {
            role,
            text: text.as_ref().map(|text| text.text.clone()),
            skill,
            assistant_phase,
            image_attachment_count,
            truncated: text.as_ref().is_some_and(|text| text.truncated),
            original_content_bytes: text.map_or(0, |text| text.original_bytes),
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn new_item(
    expected_scope: &str,
    record: &SessionStreamRecord,
    subindex: u32,
    kind: ConversationDisplayItemKindV1,
    source: ConversationDisplaySourceV1,
    run_id: Option<String>,
    status: ConversationDisplayStatusV1,
    content: ConversationDisplayContentV1,
) -> ConversationDisplayItemV1 {
    ConversationDisplayItemV1 {
        schema_version: CONVERSATION_DISPLAY_SCHEMA_VERSION,
        display_id: stable_display_id(expected_scope, record.event_id(), subindex),
        display_order: ConversationDisplayOrderV1 {
            session_stream_sequence: record.stream_sequence(),
            subindex,
        },
        source_event_id: record.event_id().to_owned(),
        kind,
        source,
        run_id,
        run_sequence: None,
        status,
        content,
        reconciles: None,
    }
}

fn stable_display_id(scope: &str, source_event_id: &str, subindex: u32) -> String {
    let mut digest = Sha256::new();
    digest.update(b"sigil-conversation-display-v1\0");
    digest.update(u64::try_from(scope.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(scope.as_bytes());
    digest.update(
        u64::try_from(source_event_id.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    digest.update(source_event_id.as_bytes());
    digest.update(subindex.to_be_bytes());
    format!("display-sha256:{:x}", digest.finalize())
}

fn scope_sha256(scope: &str) -> String {
    format!("{:x}", Sha256::digest(scope.as_bytes()))
}

fn frontier_binding_sha256(
    scope: &str,
    records: &[SessionStreamRecord],
    sequence: u64,
    before_order: ConversationDisplayOrderV1,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"sigil-conversation-display-frontier-v1\0");
    digest.update(u64::try_from(scope.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(scope.as_bytes());
    digest.update(sequence.to_be_bytes());
    digest.update(before_order.session_stream_sequence.to_be_bytes());
    digest.update(before_order.subindex.to_be_bytes());
    for record in records
        .iter()
        .take_while(|record| record.stream_sequence() <= sequence)
    {
        digest.update(record.stream_sequence().to_be_bytes());
        digest.update(
            u64::try_from(record.event_id().len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        digest.update(record.event_id().as_bytes());
        digest.update(
            u64::try_from(record.record_checksum().len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        digest.update(record.record_checksum().as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn encode_cursor(cursor: &ConversationDisplayCursorV1) -> Result<String> {
    let encoded = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(cursor).context("failed to encode conversation display cursor")?,
    );
    if encoded.len() > MAX_CONVERSATION_DISPLAY_CURSOR_BYTES {
        bail!("conversation display cursor exceeds bounded size");
    }
    Ok(encoded)
}

fn decode_cursor(encoded: &str) -> Result<ConversationDisplayCursorV1> {
    if encoded.is_empty() || encoded.len() > MAX_CONVERSATION_DISPLAY_CURSOR_BYTES {
        bail!("conversation display cursor has invalid size");
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .context("conversation display cursor is not valid base64url")?;
    serde_json::from_slice(&bytes).context("conversation display cursor payload is invalid")
}

fn bound_identity(value: &str) -> String {
    let safe = safe_persistence_text(value);
    truncate_utf8(&safe, MAX_CONVERSATION_DISPLAY_IDENTITY_BYTES).0
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

fn map_assistant_phase(
    kind: Option<AssistantMessageKind>,
) -> Option<ConversationDisplayAssistantPhaseV1> {
    match kind {
        Some(AssistantMessageKind::ToolPreamble) => {
            Some(ConversationDisplayAssistantPhaseV1::ToolPreamble)
        }
        Some(AssistantMessageKind::Progress) => Some(ConversationDisplayAssistantPhaseV1::Progress),
        Some(AssistantMessageKind::FinalAnswer) => {
            Some(ConversationDisplayAssistantPhaseV1::FinalAnswer)
        }
        Some(AssistantMessageKind::ReasoningTrace) | None => None,
    }
}

fn map_terminal_status(
    status: ConversationRunTerminalStatusV1,
) -> Result<ConversationDisplayStatusV1> {
    Ok(match status {
        ConversationRunTerminalStatusV1::Succeeded => ConversationDisplayStatusV1::Succeeded,
        ConversationRunTerminalStatusV1::Failed => ConversationDisplayStatusV1::Failed,
        ConversationRunTerminalStatusV1::Cancelled => ConversationDisplayStatusV1::Cancelled,
        ConversationRunTerminalStatusV1::Interrupted => ConversationDisplayStatusV1::Interrupted,
        ConversationRunTerminalStatusV1::Blocked => ConversationDisplayStatusV1::Blocked,
        ConversationRunTerminalStatusV1::AwaitingUserInput => {
            ConversationDisplayStatusV1::AwaitingUserInput
        }
        _ => bail!("unsupported conversation run terminal status"),
    })
}

fn map_approval_decision(
    decision: ToolApprovalUserDecision,
) -> ConversationDisplayApprovalDecisionV1 {
    match decision {
        ToolApprovalUserDecision::Approved => ConversationDisplayApprovalDecisionV1::Approved,
        ToolApprovalUserDecision::ApprovedForSession => {
            ConversationDisplayApprovalDecisionV1::ApprovedForSession
        }
        ToolApprovalUserDecision::Denied => ConversationDisplayApprovalDecisionV1::Denied,
    }
}

fn map_checkpoint_conflict_reason(
    reason: CheckpointRestoreConflictReason,
) -> ConversationDisplayCheckpointConflictReasonV1 {
    match reason {
        CheckpointRestoreConflictReason::WorkspaceMismatch => {
            ConversationDisplayCheckpointConflictReasonV1::WorkspaceMismatch
        }
        CheckpointRestoreConflictReason::CurrentHashMismatch => {
            ConversationDisplayCheckpointConflictReasonV1::CurrentHashMismatch
        }
        CheckpointRestoreConflictReason::IntentStateConflict => {
            ConversationDisplayCheckpointConflictReasonV1::IntentStateConflict
        }
        CheckpointRestoreConflictReason::ArtifactUnavailable => {
            ConversationDisplayCheckpointConflictReasonV1::ArtifactUnavailable
        }
        CheckpointRestoreConflictReason::SensitiveSnapshot => {
            ConversationDisplayCheckpointConflictReasonV1::SensitiveSnapshot
        }
        CheckpointRestoreConflictReason::UnsupportedSnapshot => {
            ConversationDisplayCheckpointConflictReasonV1::UnsupportedSnapshot
        }
        CheckpointRestoreConflictReason::InvalidBinding => {
            ConversationDisplayCheckpointConflictReasonV1::InvalidBinding
        }
    }
}

/// Incremental display projection for the bounded pending plan review surface.
#[derive(Debug, Default)]
struct PlanReviewDisplayProjection {
    attempts: Vec<sigil_kernel::PlanReviewAttemptEntry>,
    drafts: std::collections::BTreeMap<sigil_kernel::PlanId, sigil_kernel::PlanDraftCreatedEntry>,
    decisions: std::collections::BTreeMap<
        sigil_kernel::PlanId,
        Vec<sigil_kernel::PlanDecisionRecordedEntry>,
    >,
    tasks_created: std::collections::BTreeMap<
        sigil_kernel::PlanId,
        Vec<sigil_kernel::TaskCreatedFromPlanEntry>,
    >,
    // RFC-0067: durable executable candidate + ready marker; Run is only offered when both exist.
    candidates:
        std::collections::BTreeMap<sigil_kernel::PlanId, sigil_kernel::ExecutablePlanCandidateV1>,
    ready_markers:
        std::collections::BTreeMap<sigil_kernel::PlanId, sigil_kernel::PlanReadyCommittedV1Entry>,
    revision_guidance:
        std::collections::BTreeMap<sigil_kernel::UserInputIdentityV1, sigil_kernel::PlanId>,
    pending_revision_guidance: std::collections::BTreeSet<sigil_kernel::PlanId>,
    latest_revision_request:
        std::collections::BTreeMap<sigil_kernel::PlanId, sigil_kernel::UserInputRequestId>,
    revision_guidance_resolution:
        std::collections::BTreeMap<sigil_kernel::PlanId, sigil_kernel::UserInputResolutionV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanReviewCompatibilityStatusV1 {
    NoPlanReview,
    Current,
    LegacyRecovered,
    UnsupportedLegacy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LegacyPlanRevisionRecovery {
    base_attempt_index: usize,
    terminal_attempt_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LegacyPlanRevisionCompatibility {
    None,
    Recovered(LegacyPlanRevisionRecovery),
    Unsupported,
}

impl PlanReviewDisplayProjection {
    fn apply_entry(&mut self, entry: sigil_kernel::SessionLogEntry) {
        match entry {
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::PlanReviewAttempt(attempt),
            ) => self.attempts.push(attempt),
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::PlanDraftCreated(draft),
            ) => {
                self.drafts.insert(draft.plan_id.clone(), draft);
            }
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::PlanDecisionRecorded(decision),
            ) => self
                .decisions
                .entry(decision.plan_id.clone())
                .or_default()
                .push(decision),
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::TaskCreatedFromPlan(created),
            ) => self
                .tasks_created
                .entry(created.plan_id.clone())
                .or_default()
                .push(created),
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::ExecutablePlanCandidatePreparedV1(candidate),
            ) => {
                self.candidates
                    .insert(candidate.plan_id.clone(), (*candidate).clone());
            }
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::PlanReadyCommittedV1(marker),
            ) => {
                self.ready_markers.insert(marker.plan_id.clone(), marker);
            }
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::PlanExecutionAdoptedV1(adoption),
            ) => {
                // An adopted plan is no longer a pending runnable surface.
                self.decisions
                    .entry(adoption.plan_id.clone())
                    .or_default()
                    .push(sigil_kernel::PlanDecisionRecordedEntry {
                        plan_id: adoption.plan_id.clone(),
                        plan_hash: adoption.plan_hash.clone(),
                        decision: sigil_kernel::PlanDecision::Accepted,
                        decided_by: sigil_kernel::PlanDecisionActor::User,
                        decided_at_ms: adoption.adopted_at_ms,
                        reason: Some("adopted through the single execution spine".to_owned()),
                    });
                self.tasks_created
                    .entry(adoption.plan_id.clone())
                    .or_default()
                    .push(sigil_kernel::TaskCreatedFromPlanEntry {
                        plan_id: adoption.plan_id.clone(),
                        plan_hash: adoption.plan_hash.clone(),
                        task_id: adoption.task_id.clone(),
                        task_plan_version: adoption.adopted_candidate.task_plan.plan_version,
                        step_mapping: adoption.adopted_candidate.step_mapping.clone(),
                        stale_reason: None,
                        created_at_ms: adoption.adopted_at_ms,
                    });
            }
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::UserInputRequested(requested),
            ) => {
                if let sigil_kernel::UserInputSourceV1::PlanRevision { base_plan_id, .. } =
                    &requested.request.source
                {
                    self.revision_guidance
                        .insert(requested.request.identity.clone(), base_plan_id.clone());
                    self.pending_revision_guidance.insert(base_plan_id.clone());
                    self.latest_revision_request.insert(
                        base_plan_id.clone(),
                        requested.request.identity.request_id.clone(),
                    );
                }
            }
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::UserInputResolved(resolved),
            ) => {
                if let Some(base_plan_id) = self.revision_guidance.get(&resolved.identity) {
                    self.pending_revision_guidance.remove(base_plan_id);
                    self.revision_guidance_resolution
                        .insert(base_plan_id.clone(), resolved.resolution);
                }
            }
            _ => {}
        }
    }

    fn pending_user_input(&self) -> Option<sigil_kernel::PublicUserInputRequestV1> {
        self.attempts
            .iter()
            .rev()
            .find(|attempt| {
                attempt.status == sigil_kernel::PlanReviewAttemptStatus::WaitingForInput
            })
            .and_then(|attempt| attempt.pending_user_input.as_deref())
            .cloned()
    }

    fn apply_record(&mut self, record: &sigil_kernel::SessionStreamRecord) {
        let Ok(Some(entry)) = record.session_log_entry() else {
            return;
        };
        self.apply_entry(entry);
    }

    fn into_public(
        self,
        current_workspace_snapshot_id: Option<&str>,
    ) -> Option<sigil_kernel::PublicPlanReview> {
        let latest_index = self.attempts.len().checked_sub(1)?;
        let latest = &self.attempts[latest_index];
        let legacy_revision = self.legacy_revision_compatibility(latest_index);
        let legacy_recovery = match &legacy_revision {
            LegacyPlanRevisionCompatibility::Recovered(recovery) => Some(recovery),
            LegacyPlanRevisionCompatibility::None
            | LegacyPlanRevisionCompatibility::Unsupported => None,
        };
        let active_attempt = if let Some(recovery) = legacy_recovery {
            &self.attempts[recovery.base_attempt_index]
        } else if latest.revision_request_id.is_some()
            && latest.status != sigil_kernel::PlanReviewAttemptStatus::DraftReady
        {
            latest.base_plan_id.as_ref().and_then(|base_plan_id| {
                self.attempts.iter().rev().find(|attempt| {
                    &attempt.plan_id == base_plan_id
                        && attempt.status == sigil_kernel::PlanReviewAttemptStatus::DraftReady
                })
            })?
        } else {
            latest
        };
        if self
            .decisions
            .get(&active_attempt.plan_id)
            .and_then(|entries| entries.last())
            .is_some_and(|decision| {
                matches!(
                    decision.decision,
                    sigil_kernel::PlanDecision::Accepted | sigil_kernel::PlanDecision::Rejected
                )
            })
        {
            return None;
        }
        if self.tasks_created.contains_key(&active_attempt.plan_id) {
            return None;
        }
        // The attempt status always projects; draft-specific details exist only when the latest
        // attempt committed a typed draft (a Started/failed/interrupted/cancelled attempt must
        // stay visible across reloads instead of disappearing from the display).
        let draft = self.drafts.get(&active_attempt.plan_id);
        let stale = draft
            .and_then(|draft| {
                crate::plan_review_coordinator::plan_handoff_stale_reason(
                    draft.workspace_snapshot_id.as_deref(),
                    current_workspace_snapshot_id,
                )
            })
            .is_some();
        let latest_decision = self
            .decisions
            .get(&active_attempt.plan_id)
            .and_then(|entries| entries.last())
            .map(|entry| entry.decision);
        let revision_running = latest.revision_request_id.is_some()
            && matches!(
                latest.status,
                sigil_kernel::PlanReviewAttemptStatus::Started
                    | sigil_kernel::PlanReviewAttemptStatus::WaitingForInput
                    | sigil_kernel::PlanReviewAttemptStatus::Finalizing
            );
        let guidance_pending = self
            .pending_revision_guidance
            .contains(&active_attempt.plan_id);
        // RFC-0067 6.1/15.1: Run is only offered when the exact candidate and ready marker are
        // durable. Legacy or crash-incomplete DraftReady plans stay actionable through
        // Revise/Reject (explicit recompile) but never pretend to be runnable.
        let executable = self.plan_is_executable(&active_attempt.plan_id);
        let allowed_actions = if active_attempt.status
            == sigil_kernel::PlanReviewAttemptStatus::DraftReady
            && draft.is_some()
            && !revision_running
            && !guidance_pending
        {
            if !executable {
                vec![
                    sigil_kernel::PublicPlanAction::Revise,
                    sigil_kernel::PublicPlanAction::Reject,
                ]
            } else {
                match legacy_recovery.map_or(latest_decision, |_| {
                    Some(sigil_kernel::PlanDecision::RevisionFailed)
                }) {
                    Some(sigil_kernel::PlanDecision::RevisionRequested) => Vec::new(),
                    Some(sigil_kernel::PlanDecision::SavedOnly) => vec![
                        sigil_kernel::PublicPlanAction::Run,
                        sigil_kernel::PublicPlanAction::Revise,
                        sigil_kernel::PublicPlanAction::Reject,
                    ],
                    _ => vec![
                        sigil_kernel::PublicPlanAction::Run,
                        sigil_kernel::PublicPlanAction::Save,
                        sigil_kernel::PublicPlanAction::Revise,
                        sigil_kernel::PublicPlanAction::Reject,
                    ],
                }
            }
        } else if active_attempt.status == sigil_kernel::PlanReviewAttemptStatus::CompileFailed
            && draft.is_some()
            && !revision_running
            && !guidance_pending
        {
            // RFC-0067 13.1: a compile-failed plan needs changes; it cannot be run or saved.
            vec![
                sigil_kernel::PublicPlanAction::Revise,
                sigil_kernel::PublicPlanAction::Reject,
            ]
        } else {
            Vec::new()
        };
        let (summary, summary_truncated) = draft
            .map(|draft| compact_plan_review_summary(&draft.summary))
            .map_or((None, false), |(summary, truncated)| {
                (Some(summary), truncated)
            });
        Some(sigil_kernel::PublicPlanReview {
            plan_id: active_attempt.plan_id.as_str().to_owned(),
            plan_hash: draft.map(|draft| draft.plan_hash.clone()),
            status: active_attempt.status.into(),
            summary,
            summary_truncated,
            step_count: draft.map(|draft| draft.steps.len()),
            target_path_count: draft.map(|draft| draft.target_paths.len()),
            suggested_check_count: draft.map(|draft| draft.suggested_checks.len()),
            risk: draft.and_then(|draft| draft.risk.clone()),
            allowed_actions,
            source: active_attempt.source.into(),
            stale,
            revision: if let Some(recovery) = legacy_recovery {
                let terminal = &self.attempts[recovery.terminal_attempt_index];
                Some(sigil_kernel::PublicPlanRevisionSummaryV1 {
                    request_id: format!("legacy-plan-revision-{}", terminal.attempt_id.as_str()),
                    attempt_id: Some(terminal.attempt_id.as_str().to_owned()),
                    attempt_ordinal: Some(1),
                    status: match terminal.status {
                        sigil_kernel::PlanReviewAttemptStatus::Cancelled => {
                            sigil_kernel::PublicPlanRevisionStatusV1::Cancelled
                        }
                        sigil_kernel::PlanReviewAttemptStatus::CompletedWithoutDraft
                        | sigil_kernel::PlanReviewAttemptStatus::Failed
                        | sigil_kernel::PlanReviewAttemptStatus::Interrupted => {
                            sigil_kernel::PublicPlanRevisionStatusV1::Failed
                        }
                        sigil_kernel::PlanReviewAttemptStatus::Started
                        | sigil_kernel::PlanReviewAttemptStatus::WaitingForInput
                        | sigil_kernel::PlanReviewAttemptStatus::Finalizing
                        | sigil_kernel::PlanReviewAttemptStatus::DraftReady
                        | sigil_kernel::PlanReviewAttemptStatus::CompileFailed => {
                            unreachable!("legacy recovery only accepts terminal attempts")
                        }
                    },
                    terminal_reason: terminal
                        .terminal_reason
                        .map(|reason| reason.as_str().to_owned())
                        .or_else(|| Some("legacy_revision_failed".to_owned())),
                })
            } else if let Some(request_id) = latest.revision_request_id.as_ref() {
                Some(sigil_kernel::PublicPlanRevisionSummaryV1 {
                    request_id: request_id.as_str().to_owned(),
                    attempt_id: Some(latest.attempt_id.as_str().to_owned()),
                    attempt_ordinal: Some(latest.attempt_ordinal),
                    status: match latest.status {
                        sigil_kernel::PlanReviewAttemptStatus::Started => {
                            sigil_kernel::PublicPlanRevisionStatusV1::Researching
                        }
                        sigil_kernel::PlanReviewAttemptStatus::WaitingForInput => {
                            sigil_kernel::PublicPlanRevisionStatusV1::WaitingForInput
                        }
                        sigil_kernel::PlanReviewAttemptStatus::Finalizing => {
                            sigil_kernel::PublicPlanRevisionStatusV1::Finalizing
                        }
                        sigil_kernel::PlanReviewAttemptStatus::DraftReady => {
                            sigil_kernel::PublicPlanRevisionStatusV1::Succeeded
                        }
                        sigil_kernel::PlanReviewAttemptStatus::CompileFailed => {
                            sigil_kernel::PublicPlanRevisionStatusV1::Failed
                        }
                        sigil_kernel::PlanReviewAttemptStatus::Cancelled => {
                            sigil_kernel::PublicPlanRevisionStatusV1::Cancelled
                        }
                        sigil_kernel::PlanReviewAttemptStatus::CompletedWithoutDraft
                        | sigil_kernel::PlanReviewAttemptStatus::Failed
                        | sigil_kernel::PlanReviewAttemptStatus::Interrupted => {
                            sigil_kernel::PublicPlanRevisionStatusV1::Failed
                        }
                    },
                    terminal_reason: latest
                        .terminal_reason
                        .map(|reason| reason.as_str().to_owned()),
                })
            } else {
                self.latest_revision_request
                    .get(&active_attempt.plan_id)
                    .map(|request_id| sigil_kernel::PublicPlanRevisionSummaryV1 {
                        request_id: request_id.as_str().to_owned(),
                        attempt_id: None,
                        attempt_ordinal: None,
                        status: if guidance_pending {
                            sigil_kernel::PublicPlanRevisionStatusV1::AwaitingGuidance
                        } else if latest_decision
                            == Some(sigil_kernel::PlanDecision::RevisionRequested)
                        {
                            sigil_kernel::PublicPlanRevisionStatusV1::Queued
                        } else {
                            sigil_kernel::PublicPlanRevisionStatusV1::Cancelled
                        },
                        terminal_reason: (!guidance_pending
                            && latest_decision
                                != Some(sigil_kernel::PlanDecision::RevisionRequested))
                        .then(|| {
                            self.revision_guidance_resolution
                                .get(&active_attempt.plan_id)
                                .map_or_else(
                                    || "revision guidance resolved without dispatch".to_owned(),
                                    |resolution| match resolution {
                                        sigil_kernel::UserInputResolutionV1::Declined => {
                                            "revision guidance declined".to_owned()
                                        }
                                        sigil_kernel::UserInputResolutionV1::RunCancelled => {
                                            "revision guidance cancelled".to_owned()
                                        }
                                        sigil_kernel::UserInputResolutionV1::Consumed => {
                                            "revision guidance consumed without dispatch".to_owned()
                                        }
                                        sigil_kernel::UserInputResolutionV1::Failed { .. } => {
                                            "revision guidance failed".to_owned()
                                        }
                                    },
                                )
                        }),
                    })
            },
        })
    }

    fn plan_is_executable(&self, plan_id: &sigil_kernel::PlanId) -> bool {
        self.ready_markers.get(plan_id).is_some_and(|marker| {
            self.candidates
                .get(plan_id)
                .is_some_and(|candidate| candidate.candidate_hash == marker.candidate_hash)
        })
    }

    fn legacy_revision_compatibility(
        &self,
        latest_index: usize,
    ) -> LegacyPlanRevisionCompatibility {
        let Some(terminal) = self.attempts.get(latest_index) else {
            return LegacyPlanRevisionCompatibility::None;
        };
        let has_matching_legacy_base = self.attempts[..latest_index].iter().any(|attempt| {
            attempt.plan_review_id == terminal.plan_review_id
                && attempt.plan_id != terminal.plan_id
                && attempt.source == terminal.source
                && attempt.source_turn == terminal.source_turn
                && attempt.route_decision_id == terminal.route_decision_id
                && attempt.status == sigil_kernel::PlanReviewAttemptStatus::DraftReady
                && self.drafts.get(&attempt.plan_id).is_some_and(|draft| {
                    self.decisions
                        .get(&attempt.plan_id)
                        .and_then(|entries| entries.last())
                        .is_some_and(|decision| {
                            decision.decision == sigil_kernel::PlanDecision::RevisionRequested
                                && decision.plan_hash == draft.plan_hash
                        })
                })
        });
        let has_legacy_signal = terminal.revision_request_id.is_none()
            && terminal.base_plan_id.is_none()
            && terminal.base_plan_hash.is_none()
            && terminal.status.is_terminal()
            && !self.drafts.contains_key(&terminal.plan_id)
            && has_matching_legacy_base;
        if !has_legacy_signal {
            return LegacyPlanRevisionCompatibility::None;
        }

        let Some(candidate_start_index) = self.attempts.iter().position(|attempt| {
            attempt.attempt_id == terminal.attempt_id
                && attempt.plan_id == terminal.plan_id
                && attempt.status == sigil_kernel::PlanReviewAttemptStatus::Started
        }) else {
            return LegacyPlanRevisionCompatibility::Unsupported;
        };
        if candidate_start_index >= latest_index
            || self.attempts[candidate_start_index..=latest_index]
                .iter()
                .any(|attempt| {
                    attempt.attempt_id != terminal.attempt_id
                        || attempt.plan_id != terminal.plan_id
                        || attempt.plan_review_id != terminal.plan_review_id
                        || attempt.source != terminal.source
                        || attempt.source_turn != terminal.source_turn
                        || attempt.route_decision_id != terminal.route_decision_id
                        || attempt.revision_request_id.is_some()
                        || attempt.base_plan_id.is_some()
                        || attempt.base_plan_hash.is_some()
                })
        {
            return LegacyPlanRevisionCompatibility::Unsupported;
        }

        let mut candidates = self.attempts[..candidate_start_index]
            .iter()
            .enumerate()
            .filter_map(|(index, attempt)| {
                if attempt.plan_review_id != terminal.plan_review_id
                    || attempt.source != terminal.source
                    || attempt.source_turn != terminal.source_turn
                    || attempt.route_decision_id != terminal.route_decision_id
                    || attempt.status != sigil_kernel::PlanReviewAttemptStatus::DraftReady
                {
                    return None;
                }
                let draft = self.drafts.get(&attempt.plan_id)?;
                let decision = self
                    .decisions
                    .get(&attempt.plan_id)
                    .and_then(|entries| entries.last())?;
                (decision.decision == sigil_kernel::PlanDecision::RevisionRequested
                    && decision.plan_hash == draft.plan_hash
                    && decision.decided_at_ms
                        <= self.attempts[candidate_start_index].recorded_at_ms)
                    .then_some((decision.decided_at_ms, index))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| candidate.0);
        let Some((latest_decision_at, base_attempt_index)) = candidates.pop() else {
            return LegacyPlanRevisionCompatibility::Unsupported;
        };
        if candidates
            .last()
            .is_some_and(|candidate| candidate.0 == latest_decision_at)
        {
            return LegacyPlanRevisionCompatibility::Unsupported;
        }
        LegacyPlanRevisionCompatibility::Recovered(LegacyPlanRevisionRecovery {
            base_attempt_index,
            terminal_attempt_index: latest_index,
        })
    }
}

pub(crate) fn plan_review_compatibility_from_entries(
    entries: &[sigil_kernel::SessionLogEntry],
) -> PlanReviewCompatibilityStatusV1 {
    let mut projection = PlanReviewDisplayProjection::default();
    for entry in entries {
        projection.apply_entry(entry.clone());
    }
    let Some(latest_index) = projection.attempts.len().checked_sub(1) else {
        return PlanReviewCompatibilityStatusV1::NoPlanReview;
    };
    match projection.legacy_revision_compatibility(latest_index) {
        LegacyPlanRevisionCompatibility::None => PlanReviewCompatibilityStatusV1::Current,
        LegacyPlanRevisionCompatibility::Recovered(_) => {
            PlanReviewCompatibilityStatusV1::LegacyRecovered
        }
        LegacyPlanRevisionCompatibility::Unsupported => {
            PlanReviewCompatibilityStatusV1::UnsupportedLegacy
        }
    }
}

/// Projects the reducer-owned plan summary for an in-process surface that already owns a
/// validated session snapshot.
#[must_use]
pub fn public_plan_review_from_entries(
    entries: &[sigil_kernel::SessionLogEntry],
    current_workspace_snapshot_id: Option<&str>,
) -> Option<sigil_kernel::PublicPlanReview> {
    let mut projection = PlanReviewDisplayProjection::default();
    for entry in entries {
        projection.apply_entry(entry.clone());
    }
    projection.into_public(current_workspace_snapshot_id)
}

/// Projects every unresolved attention request in stable oldest-first order.
pub fn public_user_inputs_from_entries(
    entries: &[sigil_kernel::SessionLogEntry],
) -> Result<Vec<sigil_kernel::PublicUserInputRequestV1>> {
    let user_input = sigil_kernel::UserInputProjectionV1::from_session_entries(entries)?;
    let agent_user_input =
        sigil_kernel::AgentUserInputRouteProjectionV1::from_session_entries(entries)?;
    let mut plan_review = PlanReviewDisplayProjection::default();
    for entry in entries {
        plan_review.apply_entry(entry.clone());
    }
    Ok(stable_pending_user_inputs(
        user_input
            .pending()
            .map(sigil_kernel::UserInputRequestStateV1::public_view)
            .chain(plan_review.pending_user_input())
            .chain(
                agent_user_input
                    .pending()
                    .map(|route| route.request.clone()),
            ),
    ))
}

/// Compatibility projection for v1 product surfaces that only understand one request.
///
/// The selected request is the first entry in the canonical oldest-first attention queue.
pub fn public_user_input_from_entries(
    entries: &[sigil_kernel::SessionLogEntry],
) -> Result<Option<sigil_kernel::PublicUserInputRequestV1>> {
    Ok(public_user_inputs_from_entries(entries)?.into_iter().next())
}

fn stable_pending_user_inputs(
    requests: impl IntoIterator<Item = sigil_kernel::PublicUserInputRequestV1>,
) -> Vec<sigil_kernel::PublicUserInputRequestV1> {
    const MAX_ATTENTION_REQUESTS: usize = 64;
    let mut deduplicated = BTreeMap::new();
    for request in requests {
        deduplicated.insert(
            (request.identity.clone(), request.request_hash.clone()),
            request,
        );
    }
    let mut requests = deduplicated.into_values().collect::<Vec<_>>();
    requests.sort_by(|left, right| {
        left.requested_at_unix_ms
            .cmp(&right.requested_at_unix_ms)
            .then_with(|| left.identity.cmp(&right.identity))
            .then_with(|| left.request_hash.cmp(&right.request_hash))
    });
    requests.truncate(MAX_ATTENTION_REQUESTS);
    requests
}

fn compact_plan_review_summary(summary: &str) -> (String, bool) {
    use unicode_segmentation::UnicodeSegmentation;

    const COMPACT_SUMMARY_MAX_GRAPHEMES: usize = 160;
    let mut graphemes = summary.graphemes(true);
    let visible = graphemes
        .by_ref()
        .take(COMPACT_SUMMARY_MAX_GRAPHEMES)
        .collect::<Vec<_>>();
    if graphemes.next().is_none() {
        return (summary.to_owned(), false);
    }
    let mut compact = visible
        .into_iter()
        .take(COMPACT_SUMMARY_MAX_GRAPHEMES.saturating_sub(1))
        .collect::<String>();
    compact.push('…');
    (compact, true)
}

#[cfg(test)]
mod attention_queue_tests {
    use super::stable_pending_user_inputs;

    fn request(
        request_id: &str,
        source_thread_id: &str,
        requested_at_unix_ms: u64,
    ) -> anyhow::Result<sigil_kernel::PublicUserInputRequestV1> {
        Ok(sigil_kernel::PublicUserInputRequestV1 {
            identity: sigil_kernel::UserInputIdentityV1 {
                session_scope_id: sigil_kernel::SessionScopeId::new("scope")?,
                root_logical_run_id: sigil_kernel::LogicalRunId::new("root")?,
                source_thread_id: sigil_kernel::AgentThreadId::new(source_thread_id)?,
                request_id: sigil_kernel::UserInputRequestId::new(request_id)?,
                generation: 1,
                source_binding_hash: format!("sha256:{}", "a".repeat(64)),
            },
            request_hash: format!(
                "sha256:{}",
                request_id
                    .chars()
                    .next()
                    .unwrap_or('b')
                    .to_string()
                    .repeat(64)
            ),
            source: sigil_kernel::UserInputSourceV1::Agent,
            purpose: sigil_kernel::UserInputPurposeV1::Clarification,
            prompt: request_id.to_owned(),
            questions: Vec::new(),
            allowed_actions: vec![sigil_kernel::UserInputActionV1::Submit],
            requested_at_unix_ms,
            status: sigil_kernel::UserInputStatusV1::Requested,
            answer_receipt: None,
            resolution: None,
        })
    }

    #[test]
    fn pending_attention_queue_is_oldest_first_and_deduplicated_by_exact_binding()
    -> anyhow::Result<()> {
        let newer = request("b", "child-b", 20)?;
        let older = request("a", "child-a", 10)?;
        let queue = stable_pending_user_inputs([newer.clone(), older.clone(), older]);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue[0].identity.request_id.as_str(), "a");
        assert_eq!(queue[1].identity.request_id.as_str(), "b");
        Ok(())
    }
}
