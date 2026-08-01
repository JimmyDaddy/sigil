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
    last_seen_order: BTreeMap<String, u64>,
    next_order: u64,
}

impl ConversationTaskControlProjection {
    fn apply_control(&mut self, control: &ControlEntry) {
        for event in self.events.project_control(control) {
            self.apply_event(event);
        }
    }

    fn latest_unfinished(&self) -> Option<ConversationTaskControlV1> {
        self.last_seen_order
            .iter()
            .filter_map(|(task_id, order)| {
                self.tasks.get(task_id).and_then(|task| {
                    (!matches!(task.status.as_str(), "completed" | "cancelled"))
                        .then_some((order, task))
                })
            })
            .max_by_key(|(order, _)| *order)
            .map(|(_, task)| task.clone())
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
        self.next_order = self.next_order.saturating_add(1);
        self.last_seen_order
            .insert(task_id.to_owned(), self.next_order);
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
) -> std::result::Result<ConversationDisplayPageV1, ConversationDisplayProjectionError> {
    let records = JsonlSessionStore::read_event_records(session_path).with_context(|| {
        format!(
            "failed to read conversation session {}",
            session_path.display()
        )
    })?;
    let mut page = conversation_display_page_from_records(&records, expected_scope, cursor, limit)?;
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
    let mut total_items = 0_u64;
    let mut eligible_items = 0_u64;
    let mut cursor_boundary_found = decoded_cursor.is_none();

    for record in records
        .iter()
        .take_while(|record| record.stream_sequence() <= frontier.sequence)
    {
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

    Ok(ConversationDisplayPageV1 {
        schema_version: CONVERSATION_DISPLAY_SCHEMA_VERSION,
        session_scope_id: expected_scope.to_owned(),
        through_session_stream_sequence: frontier.sequence,
        terminal_frontier,
        total_items,
        items,
        next_cursor,
        has_more,
        task_control: task_control.latest_unfinished(),
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
        if let SessionLogEntry::Control(control) = &entry {
            task_control.apply_control(control);
        }
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
            let run_id = active_run_id(active_run);
            let skill = run_id
                .as_ref()
                .and_then(|run_id| run_skills.get(run_id))
                .cloned();
            project_durable_user_message(record, expected_scope, message, run_id, skill)
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
        SessionLogEntry::ToolResultV2(result) => {
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
    active_run: &Option<ActiveRunProjection>,
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
    active_run: &Option<ActiveRunProjection>,
) -> Result<Vec<ConversationDisplayItemV1>> {
    promotion.validate_for_session(expected_scope)?;
    if let Some(active) = active_run
        && active.run_id != promotion.dispatch_run_id
    {
        bail!("conversation input promotion overlaps another durable run");
    }
    project_durable_user_message(
        record,
        expected_scope,
        promotion.durable_user_message,
        Some(promotion.dispatch_run_id),
        None,
    )
}

fn project_durable_user_message(
    record: &SessionStreamRecord,
    expected_scope: &str,
    message: ModelMessage,
    run_id: Option<String>,
    skill: Option<ConversationDisplaySkillReferenceV1>,
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
    if let Some(run_id) = run_id {
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
