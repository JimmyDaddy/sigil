use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    ControlEntry, SessionLogEntry, SessionRef, TaskId, ToolAccess, ToolCall, ToolCategory,
    ToolPreviewCapability, ToolSpec,
};

pub const REQUEST_TASK_PLANNING_TOOL_NAME: &str = "request_task_planning";
pub const CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME: &str = "continue_without_task_planning";
pub const MAX_TASK_ADMISSION_REASON_CODES: usize = 5;

/// Stable model-visible policy for semantic conversation-to-task routing.
///
/// The host exposes bounded criteria and the handoff tool, but does not inspect prompt keywords or
/// make the semantic routing decision itself.
#[must_use]
pub fn task_routing_system_prompt_contract_material() -> &'static str {
    r#"You are the semantic task router for the current user turn. This is a routing-only microturn: do not answer the user or use ordinary tools.

Classify the requested outcome by its meaning, not by keywords or by whether the user explicitly mentioned tasks, plans, or agents. Judge the structure of the requested outcome, not its estimated effort or the number of files that may need to be read. Call exactly one of the two routing tools and then stop.

Call request_task_planning when fulfilling the goal clearly requires one or more of:
- coordinated changes across multiple files, components, or architectural layers that must land consistently;
- two or more independently useful requested outcomes or work streams that can be investigated or implemented separately and then combined, even when each part is small;
- a multi-stage implementation whose stages have dependencies;
- long-running or multi-part verification;
- high-risk execution that benefits from a durable reviewed plan.

Call continue_without_task_planning for one bounded outcome: an explanation, one symbol lookup, one linear call-flow trace or summary of connected code, one narrow read-only query about a single concern, or a small single-file edit that does not meet any task-planning criterion.

Multiple files alone do not require planning. A single bounded explanation, trace, or summary remains an ordinary conversation when every file read is only supporting evidence for that one result. Conversely, read-only work requires planning when the requested product contains separate component investigations, a comparison across those investigations, or a synthesis of independently useful results. If two requested parts could each produce a useful standalone result before being combined, treat them as independent work streams.

Do not inspect files, run commands, edit code, start solving the task, or produce free text in this routing microturn. The host will either start the durable planner or begin an ordinary conversation turn after your typed decision."#
}

/// Stable host-owned transition contract after the model selects ordinary conversation.
#[must_use]
pub fn direct_conversation_continuation_prompt_contract_material() -> &'static str {
    "The routing-only microturn is complete and the typed decision selected an ordinary conversation turn. Fulfill the original user request now, using the ordinary tools advertised in this request when they are needed. Do not discuss or restate the routing decision, announce future work, or stop at an intention to act. Return a final answer only after the requested outcome is complete or you can truthfully report a concrete blocker."
}

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
        description: "Request durable task planning for the current user turn only when the goal requires coordinated cross-file or cross-layer changes, dependent stages, long verification, high-risk execution, or two or more independently useful work streams whose results must be combined. Do not infer complexity from file count alone: one bounded explanation, linear trace, or summary of connected code remains an ordinary conversation. The host owns the objective, task identity, permissions, and plan."
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

/// Model-visible negative decision for the routing-only microturn.
#[must_use]
pub fn continue_without_task_planning_tool_spec() -> ToolSpec {
    ToolSpec {
        name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
        description: "Continue with an ordinary conversation turn only when the current goal has one bounded outcome, such as an explanation, one symbol lookup, one linear trace or summary of connected code, one narrow read-only query about a single concern, or a small single-file edit. Reading multiple files as evidence for that one result is still ordinary. Do not use this when separate investigations or independently useful requested results must later be compared, synthesized, or integrated, or when the goal otherwise requires coordinated stages, long verification, or high-risk execution."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "enum": ["does_not_meet_task_planning_criteria"]
                }
            },
            "required": ["reason"],
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

/// Validates the model-owned negative decision for a routing-only microturn.
///
/// # Errors
///
/// Returns an error when the call uses another tool, has unknown fields, or does not carry the
/// single bounded negative-decision reason.
pub fn validate_continue_without_task_planning_call(call: &ToolCall) -> Result<()> {
    if call.name != CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME {
        bail!("unexpected internal task routing tool {}", call.name);
    }
    let args: RawContinueWithoutTaskPlanningArgs = serde_json::from_str(&call.args_json)
        .map_err(|error| anyhow!("invalid direct conversation routing arguments: {error}"))?;
    if args.reason != DirectConversationReason::DoesNotMeetTaskPlanningCriteria {
        bail!("direct conversation routing reason is unsupported");
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct RawTaskPlanningArgs {
    reason_codes: Vec<TaskAdmissionReason>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct RawContinueWithoutTaskPlanningArgs {
    reason: DirectConversationReason,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DirectConversationReason {
    DoesNotMeetTaskPlanningCriteria,
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
