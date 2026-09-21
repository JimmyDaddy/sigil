use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    AutomaticRouteCapability, ControlEntry, SecretString, SessionLogEntry, SessionRef, TaskId,
    TaskRunStatus, ToolAccess, ToolCall, ToolCategory, ToolPreviewCapability, ToolSpec,
};

pub const START_TASK_TOOL_NAME: &str = "start_task";
pub const CONTINUE_EXISTING_TASK_TOOL_NAME: &str = "continue_existing_task";
pub const RUN_PENDING_PLAN_TOOL_NAME: &str = "run_pending_plan";
pub const MAX_TASK_TITLE_CHARS: usize = 120;

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

/// Host-owned source that admitted a durable Task.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskAdmissionTrigger {
    ExplicitTaskCommand,
    ModelRequested,
    ApprovedPlan,
    ExplicitUserDelegation,
}

/// Durable host decision for one handoff request.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskHandoffDecision {
    Accepted,
    Rejected,
}

/// Recovery-critical record proving that the current conversation turn started a durable Task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskHandoffRequestedEntry {
    pub handoff_id: TaskHandoffId,
    pub source_turn: ConversationTurnRef,
    pub trigger: TaskAdmissionTrigger,
    /// Optional model-suggested display title; the source turn remains the Task objective.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
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
pub struct TaskStartHandoffBinding {
    pub handoff_id: TaskHandoffId,
    pub task_id: TaskId,
    pub source_turn: ConversationTurnRef,
    pub parent_session_ref: SessionRef,
    pub objective: String,
    pub policy_snapshot_hash: String,
    /// Deterministic digest of the routing contract, tool surface, capability and host route
    /// facts; frozen before provider dispatch and recorded with the route decision.
    pub route_contract_fingerprint: String,
    pub requested_at_ms: u64,
    pub decided_at_ms: u64,
}

/// Host-frozen identity of the one current durable Task that a conversation turn may continue.
///
/// The model never selects a task id or a plan version. The host derives this binding before the
/// routing request is assembled, and both the kernel and the adapter revalidate it before Task
/// execution begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskContinuationHandoffBinding {
    pub task_id: TaskId,
    pub source_turn: ConversationTurnRef,
    pub task_status: TaskRunStatus,
    pub effective_capability: AutomaticRouteCapability,
    pub policy_snapshot_hash: String,
    pub route_contract_fingerprint: String,
    pub decided_at_ms: u64,
    /// Exact source prompt retained in process memory only for direct guidance review.
    pub exact_guidance: SecretString,
    pub prompt_hash: String,
    pub exact_prompt_required: bool,
    pub safe_guidance: String,
}

/// Recovery-critical receipt selecting one exact existing Task as the current run target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskContinuationSelectedEntry {
    pub task_id: TaskId,
    pub source_turn: ConversationTurnRef,
    pub task_status: TaskRunStatus,
    pub route_contract_fingerprint: String,
    /// Typed operation selected for this exact Task.
    pub control: TaskContinuationControlKind,
    pub prompt_hash: String,
    pub exact_prompt_required: bool,
    pub guidance: String,
    pub selected_at_ms: u64,
}

/// Durable, secret-free semantic kind for one existing-Task continuation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskContinuationControlKind {
    ResumeTask,
    ApplyCurrentRequestAsGuidance,
}

impl TaskContinuationSelectedEntry {
    /// Validates the secret-free continuation receipt before append or replay.
    ///
    /// Exact prompt material is intentionally unavailable here. For sensitive prompts the
    /// receipt therefore proves only the hash of the safe durable projection; the adapter must
    /// still supply and revalidate the process-local exact text before execution.
    pub fn validate_shape(&self) -> Result<()> {
        TaskId::new(self.task_id.as_str())?;
        ConversationTurnRef::new(
            self.source_turn.session_scope_id.clone(),
            self.source_turn.message_id.clone(),
            self.source_turn.logical_run_id.clone(),
        )?;
        if !matches!(
            self.task_status,
            TaskRunStatus::Started
                | TaskRunStatus::Paused
                | TaskRunStatus::Failed
                | TaskRunStatus::Interrupted
        ) {
            bail!("task continuation status is not resumable");
        }
        if self.route_contract_fingerprint.trim().is_empty() {
            bail!("task continuation route contract fingerprint is empty");
        }
        let projected = crate::project_conversation_prompt_for_persistence(&self.guidance);
        if projected.exact_prompt_required || projected.safe_prompt != self.guidance {
            bail!("task continuation guidance projection is not safe");
        }
        let safe_hash = projected
            .prompt_hash
            .strip_prefix("safe:")
            .ok_or_else(|| anyhow!("task continuation guidance hash projection is invalid"))?;
        let expected_prompt_hash = if self.exact_prompt_required {
            format!(
                "{}{}",
                crate::CONVERSATION_EXACT_PROMPT_REQUIRED_HASH_PREFIX,
                safe_hash
            )
        } else {
            format!("safe:{safe_hash}")
        };
        if self.prompt_hash != expected_prompt_hash {
            bail!("task continuation guidance does not match its prompt hash");
        }
        if self.selected_at_ms == 0 {
            bail!("task continuation selection timestamp must be non-zero");
        }
        Ok(())
    }

    /// Validates that the receipt belongs to the durable session stream.
    pub fn validate_for_session(&self, session_id: &str) -> Result<()> {
        self.validate_shape()?;
        if self.source_turn.session_scope_id != session_id {
            bail!("task continuation source turn belongs to a different session");
        }
        Ok(())
    }
}

/// Model-visible operation for the host-selected current Task.
#[must_use]
pub fn continue_existing_task_tool_spec() -> ToolSpec {
    ToolSpec {
        name: CONTINUE_EXISTING_TASK_TOOL_NAME.to_owned(),
        description: "Operate on the exact current resumable durable Task selected by the host. Choose resume_task to continue its existing objective, or apply_current_request_as_guidance to apply the user's current request to that Task. A status question can be answered directly without calling this tool. The host owns the task id, current status, optional plan version, permissions, and execution authority; do not use this for an unrelated request or to create a new Task."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["resume_task", "apply_current_request_as_guidance"]
                }
            },
            "required": ["action"],
            }),
        category: ToolCategory::Custom,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    }
}

/// Model-visible contract for starting a direct Task from the current user turn.
#[must_use]
pub fn start_task_tool_spec() -> ToolSpec {
    ToolSpec {
        name: START_TASK_TOOL_NAME.to_owned(),
        description: "Start direct durable execution of the current user request when it should continue independently of this conversation turn. This does not create a plan or invoke another planner. The host binds the exact current request, task identity, permissions, and execution authority. A short title is optional and only labels the Task in the interface."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_TASK_TITLE_CHARS,
                    "description": "Optional short display title. The Task objective is always the current user request."
                    }
                }
            }),
        category: ToolCategory::Custom,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    }
}

/// Model-visible decision to execute the exact pending Plan selected by the host.
///
/// Plan identity and content hash are deliberately absent from model arguments. The model decides
/// only whether the user's current turn semantically authorizes execution; the host binds and
/// revalidates the durable Plan.
#[must_use]
pub fn run_pending_plan_tool_spec() -> ToolSpec {
    ToolSpec {
        name: RUN_PENDING_PLAN_TOOL_NAME.to_owned(),
        description: "Execute the exact draft-ready Plan currently selected by the host only when the user's current request semantically authorizes running that Plan. Do not infer authorization from a keyword alone. The host owns the plan identity, content hash, approval state, permissions, and Task identity."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {},
            }),
        category: ToolCategory::Custom,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    }
}

/// Parses the optional display title for the direct Task start action.
///
/// # Errors
///
/// Returns an error for a malformed title. Unknown fields are ignored.
pub fn start_task_title(call: &ToolCall) -> Result<Option<String>> {
    if call.name != START_TASK_TOOL_NAME {
        bail!("unexpected internal task handoff tool {}", call.name);
    }
    let args: RawStartTaskArgs = serde_json::from_str(&call.args_json)
        .map_err(|error| anyhow!("invalid start_task arguments: {error}"))?;
    args.title
        .map(|title| {
            let title = title.trim().to_owned();
            if title.is_empty() {
                bail!("start_task title must not be empty");
            }
            if title.chars().count() > MAX_TASK_TITLE_CHARS {
                bail!("start_task title exceeds {MAX_TASK_TITLE_CHARS} characters");
            }
            Ok(title)
        })
        .transpose()
}

/// Validates the model-owned decision to continue the host-frozen current Task.
///
/// # Errors
///
/// Returns an error when the call uses another tool, has malformed known fields, or carries an
/// unsupported reason. Task identity is deliberately absent from model arguments.
pub fn validate_continue_existing_task_call(call: &ToolCall) -> Result<()> {
    continue_existing_task_control_kind(call).map(|_| ())
}

/// Parses the typed semantic continuation choice without inspecting the user's prompt text.
pub fn continue_existing_task_control_kind(call: &ToolCall) -> Result<TaskContinuationControlKind> {
    if call.name != CONTINUE_EXISTING_TASK_TOOL_NAME {
        bail!("unexpected internal task continuation tool {}", call.name);
    }
    let args: RawContinueExistingTaskArgs = serde_json::from_str(&call.args_json)
        .map_err(|error| anyhow!("invalid task continuation arguments: {error}"))?;
    Ok(args.action)
}

/// Validates the model-owned execution decision for the host-selected pending Plan.
pub fn validate_run_pending_plan_call(call: &ToolCall) -> Result<()> {
    if call.name != RUN_PENDING_PLAN_TOOL_NAME {
        bail!("unexpected internal pending plan tool {}", call.name);
    }
    let _: RawRunPendingPlanArgs = serde_json::from_str(&call.args_json)
        .map_err(|error| anyhow!("invalid pending plan arguments: {error}"))?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RawStartTaskArgs {
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RawContinueExistingTaskArgs {
    action: TaskContinuationControlKind,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RawRunPendingPlanArgs {}

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
