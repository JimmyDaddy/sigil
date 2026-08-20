use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    AutomaticRouteCapability, ControlEntry, SecretString, SessionLogEntry, SessionRef, TaskId,
    TaskPlanStatus, TaskRunStatus, ToolAccess, ToolCall, ToolCategory, ToolPreviewCapability,
    ToolSpec,
};

pub const REQUEST_TASK_PLANNING_TOOL_NAME: &str = "request_task_planning";
pub const CONTINUE_EXISTING_TASK_TOOL_NAME: &str = "continue_existing_task";
pub const CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME: &str = "continue_without_task_planning";
pub const RUN_PENDING_PLAN_TOOL_NAME: &str = "run_pending_plan";
pub const KEEP_PENDING_PLAN_TOOL_NAME: &str = "keep_pending_plan";
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
    pub plan_version: Option<u32>,
    pub task_status: TaskRunStatus,
    pub plan_status: Option<TaskPlanStatus>,
    pub effective_capability: AutomaticRouteCapability,
    pub policy_snapshot_hash: String,
    pub route_contract_fingerprint: String,
    pub decided_at_ms: u64,
    /// Exact source prompt retained in process memory only for planner guidance review.
    pub exact_guidance: SecretString,
    pub prompt_hash: String,
    pub exact_prompt_required: bool,
    pub safe_guidance: String,
}

/// Recovery-critical receipt selecting one exact existing Task as the current run target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskContinuationSelectedEntry {
    pub task_id: TaskId,
    pub source_turn: ConversationTurnRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_version: Option<u32>,
    pub task_status: TaskRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_status: Option<TaskPlanStatus>,
    pub route_contract_fingerprint: String,
    /// Typed semantic choice returned by the routing model. Older records remain explicitly
    /// unspecified; a later typed routing call may upgrade that process-local action without
    /// guessing from localized prompt text.
    #[serde(default)]
    pub control: TaskContinuationControlKind,
    pub prompt_hash: String,
    pub exact_prompt_required: bool,
    pub guidance: String,
    pub selected_at_ms: u64,
}

/// Durable, secret-free semantic kind for one existing-Task continuation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskContinuationControlKind {
    ResumeTask,
    ApplyCurrentRequestAsGuidance,
    /// Compatibility state for receipts written before the typed action field existed.
    #[default]
    LegacyUnspecified,
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
        if self.plan_version.is_some_and(|version| version == 0) {
            bail!("task continuation plan version must be non-zero");
        }
        if !matches!(
            self.task_status,
            TaskRunStatus::Started
                | TaskRunStatus::Paused
                | TaskRunStatus::Failed
                | TaskRunStatus::Interrupted
        ) {
            bail!("task continuation status is not resumable");
        }
        if self.plan_version.is_none() != self.plan_status.is_none() {
            bail!("task continuation plan version and status must be present together");
        }
        if self
            .plan_status
            .is_some_and(|status| status != TaskPlanStatus::Accepted)
        {
            bail!("task continuation plan must be accepted");
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

/// Model-visible typed decision for continuing the host-selected current Task.
#[must_use]
pub fn continue_existing_task_tool_spec() -> ToolSpec {
    ToolSpec {
        name: CONTINUE_EXISTING_TASK_TOOL_NAME.to_owned(),
        description: "Continue the exact current resumable durable Task selected by the host when the user's request semantically resumes, finishes, adjusts, or follows up on that Task. The host owns the task id, current status, plan version, permissions, and execution authority; do not use this for an unrelated request or to create a new Task."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "enum": ["continue_current_task"]
                },
                "action": {
                    "type": "string",
                    "enum": ["resume_task", "apply_current_request_as_guidance"]
                }
            },
            "required": ["reason", "action"],
            "additionalProperties": false
        }),
        category: ToolCategory::Custom,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    }
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
            "properties": {
                "reason": {
                    "type": "string",
                    "enum": ["execute_current_pending_plan"]
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

/// Model-visible negative decision for a turn that does not authorize pending Plan execution.
#[must_use]
pub fn keep_pending_plan_tool_spec() -> ToolSpec {
    ToolSpec {
        name: KEEP_PENDING_PLAN_TOOL_NAME.to_owned(),
        description: "Keep the exact host-selected pending Plan unchanged when the user's current request does not clearly authorize executing it. This prevents an unrelated, ambiguous, revision, save, or rejection request from being treated as execution."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "enum": ["execution_not_authorized"]
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

/// Validates the model-owned decision to continue the host-frozen current Task.
///
/// # Errors
///
/// Returns an error when the call uses another tool, includes unknown fields, or carries an
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
        .map_err(|error| anyhow!("invalid task continuation routing arguments: {error}"))?;
    if args.reason != ExistingTaskContinuationReason::ContinueCurrentTask {
        bail!("task continuation routing reason is unsupported");
    }
    Ok(args.action)
}

/// Validates the model-owned execution decision for the host-selected pending Plan.
pub fn validate_run_pending_plan_call(call: &ToolCall) -> Result<()> {
    validate_pending_plan_decision_call(
        call,
        RUN_PENDING_PLAN_TOOL_NAME,
        PendingPlanDecisionReason::ExecuteCurrentPendingPlan,
    )
}

/// Validates the model-owned negative decision for the host-selected pending Plan.
pub fn validate_keep_pending_plan_call(call: &ToolCall) -> Result<()> {
    validate_pending_plan_decision_call(
        call,
        KEEP_PENDING_PLAN_TOOL_NAME,
        PendingPlanDecisionReason::ExecutionNotAuthorized,
    )
}

fn validate_pending_plan_decision_call(
    call: &ToolCall,
    expected_name: &str,
    expected_reason: PendingPlanDecisionReason,
) -> Result<()> {
    if call.name != expected_name {
        bail!(
            "unexpected internal pending plan decision tool {}",
            call.name
        );
    }
    let args: RawPendingPlanDecisionArgs = serde_json::from_str(&call.args_json)
        .map_err(|error| anyhow!("invalid pending plan decision arguments: {error}"))?;
    if args.reason != expected_reason {
        bail!("pending plan decision reason is unsupported");
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct RawContinueExistingTaskArgs {
    reason: ExistingTaskContinuationReason,
    action: TaskContinuationControlKind,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct RawPendingPlanDecisionArgs {
    reason: PendingPlanDecisionReason,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DirectConversationReason {
    DoesNotMeetTaskPlanningCriteria,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ExistingTaskContinuationReason {
    ContinueCurrentTask,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PendingPlanDecisionReason {
    ExecuteCurrentPendingPlan,
    ExecutionNotAuthorized,
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
