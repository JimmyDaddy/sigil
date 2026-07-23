use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    ControlEntry, SessionLogEntry, SessionRef, TaskId, ToolAccess, ToolCall, ToolCategory,
    ToolPreviewCapability, ToolSpec,
};

pub const REQUEST_TASK_PLANNING_TOOL_NAME: &str = "request_task_planning";
pub const MAX_TASK_ADMISSION_REASON_CODES: usize = 5;

/// Stable identity for one conversation-to-task handoff.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct TaskHandoffId(String);

impl TaskHandoffId {
    /// Creates a path-safe handoff identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is not a valid stable task-style id.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        TaskId::new(value.clone())?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Durable reference to the exact user turn that owns a root conversation run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub struct ConversationTurnRef {
    pub session_scope_id: String,
    pub message_id: String,
    pub logical_run_id: String,
}

impl ConversationTurnRef {
    /// Creates a source-turn reference without persisting prompt content.
    ///
    /// # Errors
    ///
    /// Returns an error when any identity component is empty, unbounded, or contains control
    /// characters.
    pub fn new(
        session_scope_id: impl Into<String>,
        message_id: impl Into<String>,
        logical_run_id: impl Into<String>,
    ) -> Result<Self> {
        let source = Self {
            session_scope_id: session_scope_id.into(),
            message_id: message_id.into(),
            logical_run_id: logical_run_id.into(),
        };
        validate_turn_component("session scope id", &source.session_scope_id)?;
        validate_turn_component("message id", &source.message_id)?;
        validate_turn_component("logical run id", &source.logical_run_id)?;
        Ok(source)
    }
}

/// Host-owned source that admitted durable task planning.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskAdmissionTrigger {
    ExplicitTaskCommand,
    ModelRequested,
    ApprovedPlan,
    ExplicitUserDelegation,
}

/// Bounded model-provided reason for escalating one conversation to a durable task.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TaskAdmissionReason {
    CrossLayer,
    ParallelResearch,
    MultiStageChange,
    LongVerification,
    HighRisk,
}

impl TaskAdmissionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CrossLayer => "cross_layer",
            Self::ParallelResearch => "parallel_research",
            Self::MultiStageChange => "multi_stage_change",
            Self::LongVerification => "long_verification",
            Self::HighRisk => "high_risk",
        }
    }
}

/// Durable host decision for one handoff request.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskHandoffDecision {
    Accepted,
    Rejected,
}

/// Recovery-critical record proving that a typed task handoff was requested.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskHandoffRequestedEntry {
    pub handoff_id: TaskHandoffId,
    pub source_turn: ConversationTurnRef,
    pub trigger: TaskAdmissionTrigger,
    pub reason_codes: Vec<TaskAdmissionReason>,
    /// Safe source objective retained only when this single recovery-critical fact must be able to
    /// reconstruct a not-yet-written explicit `/task` User entry after a crash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_objective: Option<String>,
    pub policy_snapshot_hash: String,
    pub requested_at_ms: u64,
}

/// Recovery-critical record binding one handoff decision to a stable task identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskHandoffResolvedEntry {
    pub handoff_id: TaskHandoffId,
    pub decision: TaskHandoffDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    pub decided_at_ms: u64,
}

/// Host-bound facts required to materialize one model-requested task handoff.
///
/// The model only supplies bounded reason codes. Identity, objective, policy, parent session, and
/// timestamps are all bound before provider dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPlanningHandoffBinding {
    pub handoff_id: TaskHandoffId,
    pub task_id: TaskId,
    pub source_turn: ConversationTurnRef,
    pub parent_session_ref: SessionRef,
    pub objective: String,
    pub policy_snapshot_hash: String,
    pub requested_at_ms: u64,
    pub decided_at_ms: u64,
}

/// Model-visible schema for the internal conversation-to-task handoff tool.
#[must_use]
pub fn request_task_planning_tool_spec() -> ToolSpec {
    ToolSpec {
        name: REQUEST_TASK_PLANNING_TOOL_NAME.to_owned(),
        description: "Request durable task planning for the current user turn only when the goal requires coordinated multi-stage work, cross-layer changes, parallel research, long verification, or high-risk execution. Do not call this for simple questions, one symbol lookup, one read-only query, or a small local edit. The host owns the objective, task identity, permissions, and plan."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "reason_codes": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_TASK_ADMISSION_REASON_CODES,
                    "uniqueItems": true,
                    "items": {
                        "type": "string",
                        "enum": [
                            "cross_layer",
                            "parallel_research",
                            "multi_stage_change",
                            "long_verification",
                            "high_risk"
                        ]
                    }
                }
            },
            "required": ["reason_codes"],
            "additionalProperties": false
        }),
        category: ToolCategory::Custom,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    }
}

/// Parses the bounded model-owned portion of a task handoff request.
///
/// # Errors
///
/// Returns an error for unknown fields/reasons, empty or oversized arrays, or duplicates.
pub fn task_planning_reason_codes(call: &ToolCall) -> Result<Vec<TaskAdmissionReason>> {
    if call.name != REQUEST_TASK_PLANNING_TOOL_NAME {
        bail!("unexpected internal task handoff tool {}", call.name);
    }
    let args: RawTaskPlanningArgs = serde_json::from_str(&call.args_json)
        .map_err(|error| anyhow!("invalid task planning request arguments: {error}"))?;
    if args.reason_codes.is_empty() {
        bail!("task planning request requires at least one reason code");
    }
    if args.reason_codes.len() > MAX_TASK_ADMISSION_REASON_CODES {
        bail!("task planning request exceeds {MAX_TASK_ADMISSION_REASON_CODES} reason codes");
    }
    let unique = args.reason_codes.iter().copied().collect::<BTreeSet<_>>();
    if unique.len() != args.reason_codes.len() {
        bail!("task planning request contains duplicate reason codes");
    }
    Ok(args.reason_codes)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct RawTaskPlanningArgs {
    reason_codes: Vec<TaskAdmissionReason>,
}

/// Latest durable state for one handoff identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskHandoffProjectionEntry {
    pub request: Option<TaskHandoffRequestedEntry>,
    pub resolution: Option<TaskHandoffResolvedEntry>,
    pub duplicate_requests: usize,
    pub duplicate_resolutions: usize,
    pub conflict: Option<String>,
}

/// Independent projection for conversation-to-task admission.
///
/// Accepted handoffs deliberately do not create placeholder task runs. Only a real `TaskRun`
/// control entry makes a task visible in `TaskStateProjection`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskHandoffProjection {
    pub handoffs: BTreeMap<TaskHandoffId, TaskHandoffProjectionEntry>,
    pub source_handoffs: BTreeMap<(String, String), TaskHandoffId>,
    pub accepted_tasks: BTreeMap<TaskId, TaskHandoffId>,
    pub conflicts: Vec<String>,
}

impl TaskHandoffProjection {
    pub fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            let SessionLogEntry::Control(control) = entry else {
                continue;
            };
            projection.apply_control_entry(control);
        }
        projection
    }

    pub fn handoff_for_source(
        &self,
        source_turn: &ConversationTurnRef,
    ) -> Option<&TaskHandoffProjectionEntry> {
        self.source_handoffs
            .get(&source_identity(source_turn))
            .and_then(|handoff_id| self.handoffs.get(handoff_id))
    }

    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }

    pub(crate) fn apply_control_entry(&mut self, control: &ControlEntry) {
        match control {
            ControlEntry::TaskHandoffRequested(entry) => self.apply_requested(entry),
            ControlEntry::TaskHandoffResolved(entry) => self.apply_resolved(entry),
            _ => {}
        }
    }

    fn apply_requested(&mut self, entry: &TaskHandoffRequestedEntry) {
        let source_identity = source_identity(&entry.source_turn);
        if let Some(existing_handoff_id) = self.source_handoffs.get(&source_identity)
            && existing_handoff_id != &entry.handoff_id
        {
            let conflict = format!(
                "source turn {} has conflicting handoffs {} and {}",
                entry.source_turn.message_id,
                existing_handoff_id.as_str(),
                entry.handoff_id.as_str()
            );
            self.record_conflict(&entry.handoff_id, conflict);
            return;
        }
        self.source_handoffs
            .insert(source_identity, entry.handoff_id.clone());
        let state = self.handoffs.entry(entry.handoff_id.clone()).or_default();
        match state.request.as_ref() {
            None => state.request = Some(entry.clone()),
            Some(existing) if existing == entry => {
                state.duplicate_requests = state.duplicate_requests.saturating_add(1);
            }
            Some(_) => {
                let conflict = format!(
                    "handoff {} has conflicting request facts",
                    entry.handoff_id.as_str()
                );
                state.conflict = Some(conflict.clone());
                self.conflicts.push(conflict);
            }
        }
    }

    fn apply_resolved(&mut self, entry: &TaskHandoffResolvedEntry) {
        let invalid_shape = match entry.decision {
            TaskHandoffDecision::Accepted => entry.task_id.is_none(),
            TaskHandoffDecision::Rejected => entry.task_id.is_some(),
        };
        if invalid_shape {
            self.record_conflict(
                &entry.handoff_id,
                format!(
                    "handoff {} has an invalid resolution shape",
                    entry.handoff_id.as_str()
                ),
            );
            return;
        }

        let state = self.handoffs.entry(entry.handoff_id.clone()).or_default();
        match state.resolution.as_ref() {
            None => state.resolution = Some(entry.clone()),
            Some(existing) if existing == entry => {
                state.duplicate_resolutions = state.duplicate_resolutions.saturating_add(1);
                return;
            }
            Some(_) => {
                let conflict = format!(
                    "handoff {} has conflicting resolutions",
                    entry.handoff_id.as_str()
                );
                state.conflict = Some(conflict.clone());
                self.conflicts.push(conflict);
                return;
            }
        }

        if let Some(task_id) = entry.task_id.as_ref()
            && let Some(existing_handoff_id) = self.accepted_tasks.get(task_id)
            && existing_handoff_id != &entry.handoff_id
        {
            self.record_conflict(
                &entry.handoff_id,
                format!(
                    "task {} is bound to conflicting handoffs {} and {}",
                    task_id.as_str(),
                    existing_handoff_id.as_str(),
                    entry.handoff_id.as_str()
                ),
            );
            return;
        }
        if let Some(task_id) = entry.task_id.as_ref() {
            self.accepted_tasks
                .insert(task_id.clone(), entry.handoff_id.clone());
        }
    }

    fn record_conflict(&mut self, handoff_id: &TaskHandoffId, conflict: String) {
        self.handoffs
            .entry(handoff_id.clone())
            .or_default()
            .conflict = Some(conflict.clone());
        self.conflicts.push(conflict);
    }
}

fn source_identity(source_turn: &ConversationTurnRef) -> (String, String) {
    (
        source_turn.session_scope_id.clone(),
        source_turn.message_id.clone(),
    )
}

fn validate_turn_component(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{label} is empty");
    }
    if value.len() > 256 {
        bail!("{label} exceeds 256 bytes");
    }
    if value.chars().any(char::is_control) {
        bail!("{label} contains control characters");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/task_handoff_tests.rs"]
mod tests;
