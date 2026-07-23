use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Component, Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Digest;

use crate::{
    AgentArtifactRef, AgentFinalAnswerRef,
    provider::ToolCall,
    session::{ControlEntry, SessionLogEntry},
    tool::{ToolAccess, ToolCategory, ToolPreviewCapability, ToolSpec},
};

pub const TASK_PLAN_UPDATE_TOOL_NAME: &str = "task_plan_update";
/// Maximum number of characters copied from a participant transcript into parent task control.
pub const TASK_PARTICIPANT_RESULT_SUMMARY_MAX_CHARS: usize = 4_000;
/// Maximum artifact references copied from one participant into parent task control.
pub const TASK_PARTICIPANT_RESULT_ARTIFACT_MAX_ITEMS: usize = 16;
/// Maximum changed paths copied from one participant into parent task control.
pub const TASK_PARTICIPANT_RESULT_CHANGED_PATH_MAX_ITEMS: usize = 64;
/// Maximum verification references copied from one participant into parent task control.
pub const TASK_PARTICIPANT_RESULT_VERIFICATION_REF_MAX_ITEMS: usize = 32;
/// Maximum characters retained for one participant result reference field.
pub const TASK_PARTICIPANT_RESULT_REF_MAX_CHARS: usize = 1_024;
/// Maximum characters retained for the short kind of an artifact reference.
pub const TASK_PARTICIPANT_RESULT_ARTIFACT_KIND_MAX_CHARS: usize = 128;
/// Maximum automatic provider-pressure retries for one task participant identity.
pub const MAX_TASK_PARTICIPANT_AUTO_RETRIES: usize = 2;
/// Maximum cumulative delay admitted for automatic retries of one participant identity.
pub const MAX_TASK_PARTICIPANT_AUTO_RETRY_WAIT_MS: u64 = 120_000;

const TASK_PARTICIPANT_ATTEMPT_ID_DOMAIN: &str = "sigil-task-participant-attempt-v1";
const TASK_PARTICIPANT_CHILD_ID_DOMAIN: &str = "sigil-task-participant-child-v1";
const TASK_FINAL_MESSAGE_ID_DOMAIN: &str = "sigil-task-final-message-v1";

/// Stable logical-run correlation for the planner attempt owned by one durable task.
#[must_use]
pub fn task_planner_logical_run_id(task_id: &TaskId) -> String {
    format!("task-planner:{}", task_id.as_str())
}

/// Stable logical-run correlation for one participant physical attempt.
#[must_use]
pub fn task_participant_logical_run_id(attempt_id: &TaskParticipantAttemptId) -> String {
    format!("task-participant:{}", attempt_id.as_str())
}
/// Small bounded replan budget for one task planning run.
pub const DEFAULT_TASK_MAX_PLAN_VERSIONS: usize = 3;
/// Maximum number of Unicode scalar values allowed in a user-facing task agent display name.
pub const TASK_AGENT_DISPLAY_NAME_MAX_CHARS: usize = 32;

/// Stable identifier for one durable task run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct TaskId(String);

impl TaskId {
    /// Creates a task identifier that is safe to embed in control state and relative paths.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty or contains path separators or unstable
    /// characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("task id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identifier for one task step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct TaskStepId(String);

impl TaskStepId {
    /// Creates a path-safe task step identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty or contains path separators or unstable
    /// characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("task step id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identifier for one planner, executable-step, or synthesis attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct TaskParticipantAttemptId(String);

impl TaskParticipantAttemptId {
    /// Creates a path-safe participant attempt identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is empty or unstable.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("task participant attempt id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Compatibility name used by step-specific orchestration code and RFC language.
pub type TaskStepAttemptId = TaskParticipantAttemptId;

/// Participant phase owned by one isolated transcript.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaskParticipantPurpose {
    Planner,
    Step,
    Synthesis,
}

impl TaskParticipantPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planner => "planner",
            Self::Step => "step",
            Self::Synthesis => "synthesis",
        }
    }
}

/// Durable lifecycle for a participant attempt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskParticipantAttemptStatus {
    Started,
    Completed,
    Failed,
    Blocked,
    Cancelled,
    Interrupted,
}

impl TaskParticipantAttemptStatus {
    pub fn is_terminal(self) -> bool {
        self != Self::Started
    }
}

/// Builds the stable identity for one participant retry.
///
/// # Errors
///
/// Returns an error when the resulting identifier cannot be represented safely.
pub fn task_participant_attempt_id(
    task_id: &TaskId,
    purpose: TaskParticipantPurpose,
    plan_version: Option<u32>,
    step_id: Option<&TaskStepId>,
    ordinal: u32,
) -> Result<TaskParticipantAttemptId> {
    if ordinal == 0 {
        bail!("task participant attempt ordinal must start at one");
    }
    let plan = plan_version.map_or_else(|| "-".to_owned(), |value| value.to_string());
    let step = step_id.map_or("-", TaskStepId::as_str);
    let digest = task_domain_hash(
        TASK_PARTICIPANT_ATTEMPT_ID_DOMAIN,
        &[
            task_id.as_str(),
            purpose.as_str(),
            &plan,
            step,
            &ordinal.to_string(),
        ],
    );
    TaskParticipantAttemptId::new(format!("attempt-{}", &digest[..24]))
}

/// Builds the child-session reference owned by one participant attempt.
///
/// # Errors
///
/// Returns an error when the resulting relative path is invalid.
pub fn task_participant_session_ref(
    task_id: &TaskId,
    attempt_id: &TaskParticipantAttemptId,
) -> Result<SessionRef> {
    SessionRef::new_relative(
        PathBuf::from("children")
            .join(task_id.as_str())
            .join(format!("{}.jsonl", attempt_id.as_str())),
    )
}

/// Builds the supervisor child task identity owned by one participant attempt.
///
/// # Errors
///
/// Returns an error when the resulting identifier is invalid.
pub fn task_participant_child_task_id(
    task_id: &TaskId,
    attempt_id: &TaskParticipantAttemptId,
) -> Result<TaskId> {
    let digest = task_domain_hash(
        TASK_PARTICIPANT_CHILD_ID_DOMAIN,
        &[task_id.as_str(), attempt_id.as_str()],
    );
    TaskId::new(format!("child-{}", &digest[..24]))
}

/// Stable parent Assistant message identity for a committed synthesis attempt.
#[must_use]
pub fn task_final_message_id(task_id: &TaskId, attempt_id: &TaskParticipantAttemptId) -> String {
    let digest = task_domain_hash(
        TASK_FINAL_MESSAGE_ID_DOMAIN,
        &[task_id.as_str(), attempt_id.as_str()],
    );
    format!("task-final-{}", &digest[..24])
}

/// Produces the bounded, persistence-safe result summary stored in the parent control log.
#[must_use]
pub fn bounded_task_participant_summary(value: &str) -> String {
    crate::safe_persistence_text(value)
        .trim()
        .chars()
        .take(TASK_PARTICIPANT_RESULT_SUMMARY_MAX_CHARS)
        .collect()
}

/// Stable identifier for an approval or elicitation route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct TaskRouteId(String);

impl TaskRouteId {
    /// Creates a route identifier used to match UI decisions to parent or child runs.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty or contains path separators or unstable
    /// characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("task route id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Session reference stored in task control entries.
///
/// The path is relative to the parent session directory. This keeps session logs portable across
/// machines and prevents child session links from escaping the session store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub struct SessionRef {
    path: String,
}

impl SessionRef {
    /// Creates a relative session reference.
    ///
    /// # Errors
    ///
    /// Returns an error when `path` is absolute, empty, or contains parent-directory traversal.
    pub fn new_relative(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        validate_relative_session_path(path)?;
        Ok(Self {
            path: path.to_string_lossy().into_owned(),
        })
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.path)
    }

    /// Resolves this reference against a parent session directory.
    pub fn resolve(&self, parent_session_dir: &Path) -> PathBuf {
        parent_session_dir.join(self.as_path())
    }
}

/// Builds a stable child session reference for a task step.
///
/// # Errors
///
/// Returns an error when any identifier is not path-safe.
pub fn child_session_ref(
    task_id: &TaskId,
    step_id: &TaskStepId,
    child_task_id: &TaskId,
) -> Result<SessionRef> {
    SessionRef::new_relative(
        PathBuf::from("children")
            .join(task_id.as_str())
            .join(format!(
                "{}-{}.jsonl",
                step_id.as_str(),
                child_task_id.as_str()
            )),
    )
}

/// Role used for a task participant.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Planner,
    Executor,
    SubagentRead,
    SubagentWrite,
}

impl AgentRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planner => "planner",
            Self::Executor => "executor",
            Self::SubagentRead => "subagent_read",
            Self::SubagentWrite => "subagent_write",
        }
    }
}

/// Durable task run status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskRunStatus {
    Started,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl TaskRunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }

    fn is_final(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }
}

/// Durable task plan status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskPlanStatus {
    Proposed,
    Accepted,
    Superseded,
    Rejected,
}

/// Durable task step status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStepStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Blocked,
    Cancelled,
    Interrupted,
    Superseded,
}

/// Runtime intent for a task graph step.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaskStepMode {
    Read,
    Write,
    Review,
    Verify,
}

impl TaskStepMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Review => "review",
            Self::Verify => "verify",
        }
    }

    fn default_for_role(role: AgentRole) -> Self {
        match role {
            AgentRole::Planner | AgentRole::SubagentRead => Self::Read,
            AgentRole::Executor | AgentRole::SubagentWrite => Self::Write,
        }
    }
}

/// Workspace isolation contract for a task graph step.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaskIsolationMode {
    SharedReadOnly,
    SequentialWorkspaceWrite,
    ChangesetOnly,
    Worktree,
}

impl TaskIsolationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SharedReadOnly => "shared_read_only",
            Self::SequentialWorkspaceWrite => "sequential_workspace_write",
            Self::ChangesetOnly => "changeset_only",
            Self::Worktree => "worktree",
        }
    }

    pub(crate) fn default_for_mode(mode: TaskStepMode) -> Self {
        match mode {
            TaskStepMode::Read | TaskStepMode::Review | TaskStepMode::Verify => {
                Self::SharedReadOnly
            }
            TaskStepMode::Write => Self::SequentialWorkspaceWrite,
        }
    }

    fn is_write_isolation(self) -> bool {
        matches!(
            self,
            Self::SequentialWorkspaceWrite | Self::ChangesetOnly | Self::Worktree
        )
    }
}

impl TaskStepStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::Blocked
                | Self::Cancelled
                | Self::Interrupted
                | Self::Superseded
        )
    }

    fn is_final(self) -> bool {
        matches!(self, Self::Completed | Self::Superseded)
    }
}

/// Durable child session status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskChildSessionStatus {
    Started,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Unavailable,
}

/// Durable route status for parent-child approval and elicitation routing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskRouteStatus {
    Registered,
    Requested,
    Resolved,
    Rejected,
    Cancelled,
    Stale,
}

/// One planned step payload stored inside a task plan entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskStepSpec {
    pub step_id: TaskStepId,
    pub title: String,
    /// Optional presentation-only child agent name for this step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub role: AgentRole,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<TaskStepId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<TaskStepMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<TaskIsolationMode>,
}

impl TaskStepSpec {
    pub fn effective_mode(&self) -> TaskStepMode {
        self.mode
            .unwrap_or_else(|| TaskStepMode::default_for_role(self.role))
    }

    pub fn effective_isolation(&self) -> TaskIsolationMode {
        self.isolation
            .unwrap_or_else(|| TaskIsolationMode::default_for_mode(self.effective_mode()))
    }

    pub fn is_review_advisory(&self) -> bool {
        self.effective_mode() == TaskStepMode::Review
    }

    pub fn requires_system_verifier(&self) -> bool {
        self.effective_mode() == TaskStepMode::Verify
    }
}

/// Bound task context for the internal planner tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskPlanUpdateContext {
    pub task_id: TaskId,
    pub max_plan_steps: usize,
    pub max_plan_versions: usize,
}

/// Model-visible schema for the internal planner plan-update tool.
pub fn task_plan_update_tool_spec() -> ToolSpec {
    ToolSpec {
        name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
        description: "Create or replace the current durable task plan. Use this before executing task steps. Do not call task, subagent, or other delegation tools. Use executor for ordinary main-session reads and edits. Use subagent_read only for delegated read-only work. Use subagent_write only for delegated changeset-only write proposals."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_version": {
                    "type": "integer",
                    "minimum": 1
                },
                "status": {
                    "type": "string",
                    "enum": ["proposed", "accepted"]
                },
                "steps": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "step_id": {
                                "type": "string",
                                "description": "Stable id using only letters, digits, dash, or underscore."
                            },
                            "title": {"type": "string"},
                            "display_name": {
                                "type": "string",
                                "description": "Optional short presentation-only name for a child agent spawned from this step. Prefer explicit configured agent or nickname names; do not use this as an identifier."
                            },
                            "detail": {"type": "string"},
                            "role": {
                                "type": "string",
                                "enum": ["planner", "executor", "subagent_read", "subagent_write"],
                                "description": "Use executor for ordinary main-session work, including sequential_workspace_write edits. Use subagent_read for delegated read-only verification. Use subagent_write only with changeset_only isolation for a delegated write proposal."
                            },
                            "depends_on": {
                                "type": "array",
                                "items": {
                                    "type": "string",
                                    "description": "Step id that must complete before this step is ready."
                                },
                                "description": "Explicit DAG dependencies. Omit or use [] for an independent step."
                            },
                            "mode": {
                                "type": "string",
                                "enum": ["read", "write", "review", "verify"],
                                "description": "Optional step intent. Omit when the role default is enough. Reviewer output is advisory; verify steps are still bound to system verification."
                            },
                            "isolation": {
                                "type": "string",
                                "enum": ["shared_read_only", "sequential_workspace_write", "changeset_only", "worktree"],
                                "description": "Optional workspace isolation contract. Omit unless a non-default is required. Write steps default to sequential_workspace_write for executor. subagent_write requires changeset_only. Read/review/verify steps always use shared_read_only."
                            }
                        },
                        "required": ["step_id", "title", "role"],
                        "additionalProperties": false
                    }
                },
                "reason": {"type": "string"}
            },
            "required": ["plan_version", "status", "steps"],
            "additionalProperties": false
        }),
        category: ToolCategory::Custom,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    }
}

/// Parses one internal `task_plan_update` call into a durable task plan entry.
///
/// # Errors
///
/// Returns an error when JSON arguments are invalid, exceed limits, or contain unsupported ids.
pub fn task_plan_update_entry(
    context: &TaskPlanUpdateContext,
    call: &ToolCall,
) -> Result<TaskPlanEntry> {
    if call.name != TASK_PLAN_UPDATE_TOOL_NAME {
        bail!("unexpected internal task tool {}", call.name);
    }
    let args: RawTaskPlanUpdateArgs = serde_json::from_str(&call.args_json)
        .map_err(|error| anyhow!("invalid task plan update arguments: {error}"))?;
    if args.plan_version == 0 {
        bail!("task plan version must be at least 1");
    }
    if usize::try_from(args.plan_version).unwrap_or(usize::MAX) > context.max_plan_versions {
        bail!(
            "task plan version {} exceeds maximum {}",
            args.plan_version,
            context.max_plan_versions
        );
    }
    if args.steps.is_empty() {
        bail!("task plan must contain at least one step");
    }
    if args.steps.len() > context.max_plan_steps {
        bail!(
            "task plan contains {} steps, maximum is {}",
            args.steps.len(),
            context.max_plan_steps
        );
    }
    let steps = args
        .steps
        .into_iter()
        .map(|step| {
            let display_name = match step.display_name.as_deref() {
                Some(display_name) => {
                    let normalized =
                        normalize_task_agent_display_name(display_name).map_err(|error| {
                            anyhow!("invalid display_name for step {}: {error}", step.step_id)
                        })?;
                    Some(
                        normalize_task_agent_display_name(&crate::safe_persistence_text(
                            &normalized,
                        ))
                        .map_err(|error| {
                            anyhow!("invalid display_name for step {}: {error}", step.step_id)
                        })?,
                    )
                }
                None => None,
            };
            let mode = step
                .mode
                .unwrap_or_else(|| TaskStepMode::default_for_role(step.role));
            let isolation = canonical_task_plan_update_isolation(mode, step.isolation);
            Ok(TaskStepSpec {
                step_id: TaskStepId::new(step.step_id)?,
                title: crate::safe_persistence_text(&step.title),
                display_name,
                detail: step.detail.as_deref().map(crate::safe_persistence_text),
                role: step.role,
                depends_on: step
                    .depends_on
                    .into_iter()
                    .map(TaskStepId::new)
                    .collect::<Result<Vec<_>>>()?,
                mode: Some(mode),
                isolation: Some(isolation),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    validate_task_plan_graph_steps(&steps)?;
    Ok(TaskPlanEntry {
        task_id: context.task_id.clone(),
        plan_version: args.plan_version,
        status: args.status,
        steps,
        reason: args.reason.as_deref().map(crate::safe_persistence_text),
    })
}

fn canonical_task_plan_update_isolation(
    mode: TaskStepMode,
    isolation: Option<TaskIsolationMode>,
) -> TaskIsolationMode {
    match mode {
        TaskStepMode::Write => isolation
            .filter(|isolation| isolation.is_write_isolation())
            .unwrap_or(TaskIsolationMode::SequentialWorkspaceWrite),
        TaskStepMode::Read | TaskStepMode::Review | TaskStepMode::Verify => {
            TaskIsolationMode::SharedReadOnly
        }
    }
}

/// Bounded model-visible response content for `task_plan_update`.
pub fn task_plan_update_result_content(entry: &TaskPlanEntry) -> String {
    json!({
        "task_id": entry.task_id.as_str(),
        "plan_version": entry.plan_version,
        "status": entry.status,
        "steps": entry.steps.len(),
        "next_action": "stop; the system orchestrator will run accepted plan steps"
    })
    .to_string()
}

/// Validates DAG metadata carried by task plan steps.
///
/// # Errors
///
/// Returns an error when step ids are duplicated, dependencies reference missing steps, the graph
/// contains a cycle, or a step declares an isolation mode incompatible with its mode.
pub fn validate_task_plan_graph_steps(steps: &[TaskStepSpec]) -> Result<()> {
    let mut step_index = HashMap::<TaskStepId, usize>::new();
    for (index, step) in steps.iter().enumerate() {
        if step_index.insert(step.step_id.clone(), index).is_some() {
            bail!("duplicate task step id {}", step.step_id.as_str());
        }
        let mode = step.effective_mode();
        let isolation = step.effective_isolation();
        validate_step_mode_isolation(&step.step_id, mode, isolation)?;
        validate_step_role_isolation(&step.step_id, step.role, isolation)?;
    }

    for step in steps {
        let mut dependencies = BTreeSet::new();
        for dependency in &step.depends_on {
            if dependency == &step.step_id {
                bail!(
                    "task step {} cannot depend on itself",
                    step.step_id.as_str()
                );
            }
            if !step_index.contains_key(dependency) {
                bail!(
                    "task step {} depends on missing step {}",
                    step.step_id.as_str(),
                    dependency.as_str()
                );
            }
            if !dependencies.insert(dependency) {
                bail!(
                    "task step {} repeats dependency {}",
                    step.step_id.as_str(),
                    dependency.as_str()
                );
            }
        }
    }

    let mut marks = vec![VisitMark::Unvisited; steps.len()];
    for index in 0..steps.len() {
        visit_task_step(index, steps, &step_index, &mut marks)?;
    }
    Ok(())
}

fn validate_step_mode_isolation(
    step_id: &TaskStepId,
    mode: TaskStepMode,
    isolation: TaskIsolationMode,
) -> Result<()> {
    if mode == TaskStepMode::Write {
        if isolation == TaskIsolationMode::SharedReadOnly {
            bail!(
                "write task step {} cannot use shared_read_only isolation",
                step_id.as_str()
            );
        }
        return Ok(());
    }
    if isolation.is_write_isolation() {
        bail!(
            "{mode} task step {} cannot use write isolation {isolation}",
            step_id.as_str(),
            mode = mode.as_str(),
            isolation = isolation.as_str()
        );
    }
    Ok(())
}

fn validate_step_role_isolation(
    step_id: &TaskStepId,
    role: AgentRole,
    isolation: TaskIsolationMode,
) -> Result<()> {
    if role == AgentRole::SubagentWrite && isolation != TaskIsolationMode::ChangesetOnly {
        bail!(
            "subagent_write task step {} requires changeset_only isolation; use executor for sequential_workspace_write edits",
            step_id.as_str()
        );
    }
    if role != AgentRole::SubagentWrite && isolation == TaskIsolationMode::ChangesetOnly {
        bail!(
            "changeset_only task step {} requires subagent_write role",
            step_id.as_str()
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisitMark {
    Unvisited,
    Visiting,
    Visited,
}

fn visit_task_step(
    index: usize,
    steps: &[TaskStepSpec],
    step_index: &HashMap<TaskStepId, usize>,
    marks: &mut [VisitMark],
) -> Result<()> {
    match marks[index] {
        VisitMark::Visited => return Ok(()),
        VisitMark::Visiting => {
            bail!("task plan contains a dependency cycle");
        }
        VisitMark::Unvisited => {}
    }

    marks[index] = VisitMark::Visiting;
    for dependency in &steps[index].depends_on {
        let Some(dependency_index) = step_index.get(dependency).copied() else {
            continue;
        };
        visit_task_step(dependency_index, steps, step_index, marks)?;
    }
    marks[index] = VisitMark::Visited;
    Ok(())
}

fn deserialize_task_plan_status<'de, D>(
    deserializer: D,
) -> std::result::Result<TaskPlanStatus, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    match value.as_str() {
        "proposed" => Ok(TaskPlanStatus::Proposed),
        "accepted" => Ok(TaskPlanStatus::Accepted),
        other => Err(serde::de::Error::custom(format!(
            "unsupported task plan status {other}"
        ))),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct RawTaskPlanUpdateArgs {
    pub plan_version: u32,
    #[serde(deserialize_with = "deserialize_task_plan_status")]
    pub status: TaskPlanStatus,
    pub steps: Vec<RawTaskStepSpec>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct RawTaskStepSpec {
    pub step_id: String,
    pub title: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    pub role: AgentRole,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub mode: Option<TaskStepMode>,
    #[serde(default)]
    pub isolation: Option<TaskIsolationMode>,
}

/// Append-only task run lifecycle entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskRunEntry {
    pub task_id: TaskId,
    pub parent_session_ref: SessionRef,
    pub objective: String,
    pub status: TaskRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Binds one concrete task-run incarnation to its root cancellation scope.
///
/// A later binding supersedes earlier scopes for the same task, allowing an explicit Continue to
/// recover normally after an older run was cancelled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskRunCancellationScopeBoundEntry {
    pub task_id: TaskId,
    pub run_scope_id: String,
}

/// Append-only task plan entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskPlanEntry {
    pub task_id: TaskId,
    pub plan_version: u32,
    pub status: TaskPlanStatus,
    #[serde(default)]
    pub steps: Vec<TaskStepSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Append-only task step lifecycle entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskStepEntry {
    pub task_id: TaskId,
    pub plan_version: u32,
    pub step_id: TaskStepId,
    pub role: AgentRole,
    pub status: TaskStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Append-only lifecycle record for an isolated task participant transcript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskParticipantAttemptEntry {
    pub attempt_id: TaskParticipantAttemptId,
    pub task_id: TaskId,
    pub purpose: TaskParticipantPurpose,
    pub ordinal: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<TaskStepId>,
    pub role: AgentRole,
    pub child_session_ref: SessionRef,
    pub status: TaskParticipantAttemptStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl TaskParticipantAttemptEntry {
    /// Validates purpose-specific identity fields before durable append or replay.
    ///
    /// # Errors
    ///
    /// Returns an error when planner, step, or synthesis facts are inconsistent.
    pub fn validate_shape(&self) -> Result<()> {
        if self.ordinal == 0 {
            bail!("task participant attempt ordinal must start at one");
        }
        match self.purpose {
            TaskParticipantPurpose::Planner => {
                if self.plan_version.is_some()
                    || self.step_id.is_some()
                    || self.role != AgentRole::Planner
                {
                    bail!("planner participant attempt has invalid plan or role facts");
                }
            }
            TaskParticipantPurpose::Step => {
                if self.plan_version.is_none() || self.step_id.is_none() {
                    bail!("step participant attempt is missing plan or step identity");
                }
            }
            TaskParticipantPurpose::Synthesis => {
                if self.plan_version.is_none()
                    || self.step_id.is_some()
                    || self.role != AgentRole::Planner
                {
                    bail!("synthesis participant attempt has invalid plan or role facts");
                }
            }
        }
        let expected = task_participant_attempt_id(
            &self.task_id,
            self.purpose,
            self.plan_version,
            self.step_id.as_ref(),
            self.ordinal,
        )?;
        if self.attempt_id != expected {
            bail!("task participant attempt id conflicts with its durable identity facts");
        }
        let expected_ref = task_participant_session_ref(&self.task_id, &self.attempt_id)?;
        if self.child_session_ref != expected_ref {
            bail!("task participant attempt child session ref is not deterministic");
        }
        Ok(())
    }
}

/// Durable proof that a provider-pressure retry cannot duplicate model output, tool work, or an
/// external effect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TaskParticipantRetryProof {
    /// A child-session physical attempt reached a synced no-consumption terminal.
    ProviderConfirmedNoConsumption {
        physical_attempt_id: String,
        request_material_fingerprint: String,
        zero_output: bool,
        zero_tool: bool,
        zero_effect: bool,
    },
    /// Runtime admission rejected the child before any provider dispatch or child start.
    AdmissionRejectedBeforeDispatch {
        zero_output: bool,
        zero_tool: bool,
        zero_effect: bool,
    },
}

impl TaskParticipantRetryProof {
    /// Validates that all three safety facts are explicit and that referenced provider evidence is
    /// structurally safe.
    ///
    /// # Errors
    ///
    /// Returns an error when any zero-effect fact is false or an evidence fingerprint is invalid.
    pub fn validate_shape(&self) -> Result<()> {
        let (zero_output, zero_tool, zero_effect) = match self {
            Self::ProviderConfirmedNoConsumption {
                physical_attempt_id,
                request_material_fingerprint,
                zero_output,
                zero_tool,
                zero_effect,
            } => {
                validate_stable_id("provider physical attempt id", physical_attempt_id)?;
                validate_prefixed_sha256(
                    "provider request material fingerprint",
                    request_material_fingerprint,
                    "hmac-sha256:",
                )?;
                (*zero_output, *zero_tool, *zero_effect)
            }
            Self::AdmissionRejectedBeforeDispatch {
                zero_output,
                zero_tool,
                zero_effect,
            } => (*zero_output, *zero_tool, *zero_effect),
        };
        if !zero_output || !zero_tool || !zero_effect {
            bail!("task participant retry proof must establish zero output, tool, and effect");
        }
        Ok(())
    }
}

/// Durable retry admission written after one failed attempt and before its replacement starts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskParticipantRetryScheduledEntry {
    pub task_id: TaskId,
    pub failed_attempt_id: TaskParticipantAttemptId,
    pub retry_attempt_id: TaskParticipantAttemptId,
    pub purpose: TaskParticipantPurpose,
    pub retry_ordinal: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<TaskStepId>,
    pub route_fingerprint: String,
    pub input_hash: String,
    pub scheduled_at_unix_ms: u64,
    pub not_before_unix_ms: u64,
    pub retry_after_ms: u64,
    pub proof: TaskParticipantRetryProof,
}

impl TaskParticipantRetryScheduledEntry {
    /// Validates deterministic retry identity, bounded timing, and zero-effect evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when identity, timing, route, input, or proof facts are inconsistent.
    pub fn validate_shape(&self) -> Result<()> {
        if self.retry_ordinal < 2 {
            bail!("task participant retry ordinal must be greater than one");
        }
        let expected = task_participant_attempt_id(
            &self.task_id,
            self.purpose,
            self.plan_version,
            self.step_id.as_ref(),
            self.retry_ordinal,
        )?;
        if self.retry_attempt_id != expected {
            bail!("task participant retry id conflicts with its durable identity facts");
        }
        if self.retry_after_ms == 0 || self.retry_after_ms > MAX_TASK_PARTICIPANT_AUTO_RETRY_WAIT_MS
        {
            bail!("task participant retry delay is outside the bounded automatic retry budget");
        }
        if self.scheduled_at_unix_ms == 0
            || self.not_before_unix_ms
                != self
                    .scheduled_at_unix_ms
                    .saturating_add(self.retry_after_ms)
        {
            bail!("task participant retry timing is inconsistent");
        }
        validate_sha256_fingerprint("provider route fingerprint", &self.route_fingerprint)?;
        validate_hex_sha256("task participant input hash", &self.input_hash)?;
        self.proof.validate_shape()
    }
}

/// Bounded result committed from a participant-owned transcript into the parent task log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskParticipantResultEntry {
    pub attempt_id: TaskParticipantAttemptId,
    pub task_id: TaskId,
    pub summary: String,
    pub summary_hash: String,
    pub output_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_status: Option<TaskParticipantAttemptStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_answer_ref: Option<AgentFinalAnswerRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<AgentArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification_refs: Vec<String>,
}

impl TaskParticipantResultEntry {
    /// Validates the bounded result and its content hash.
    ///
    /// # Errors
    ///
    /// Returns an error when the summary is oversized, unsafe, empty, or hash-inconsistent.
    pub fn validate_shape(&self) -> Result<()> {
        let bounded = bounded_task_participant_summary(&self.summary);
        if bounded.is_empty() {
            bail!("task participant result summary cannot be empty");
        }
        if bounded != self.summary {
            bail!("task participant result summary is not safely bounded");
        }
        let expected_hash = format!("sha256:{}", task_text_hash(&self.summary));
        if self.summary_hash != expected_hash {
            bail!("task participant result summary hash does not match its content");
        }
        if !self.output_hash.starts_with("sha256:") || self.output_hash.len() != 71 {
            bail!("task participant result output hash is invalid");
        }
        if self
            .terminal_status
            .is_some_and(|status| status == TaskParticipantAttemptStatus::Started)
        {
            bail!("task participant result terminal status cannot be started");
        }
        if self.artifact_refs.len() > TASK_PARTICIPANT_RESULT_ARTIFACT_MAX_ITEMS {
            bail!("task participant result has too many artifact refs");
        }
        for artifact in &self.artifact_refs {
            validate_bounded_participant_result_field(
                "artifact kind",
                &artifact.kind,
                TASK_PARTICIPANT_RESULT_ARTIFACT_KIND_MAX_CHARS,
            )?;
            validate_bounded_participant_result_field(
                "artifact path",
                &artifact.path,
                TASK_PARTICIPANT_RESULT_REF_MAX_CHARS,
            )?;
            if let Some(hash) = artifact.hash.as_deref() {
                validate_bounded_participant_result_field(
                    "artifact hash",
                    hash,
                    TASK_PARTICIPANT_RESULT_REF_MAX_CHARS,
                )?;
            }
        }
        if self.changed_paths.len() > TASK_PARTICIPANT_RESULT_CHANGED_PATH_MAX_ITEMS {
            bail!("task participant result has too many changed paths");
        }
        for path in &self.changed_paths {
            validate_bounded_participant_result_field(
                "changed path",
                path,
                TASK_PARTICIPANT_RESULT_REF_MAX_CHARS,
            )?;
        }
        if self.verification_refs.len() > TASK_PARTICIPANT_RESULT_VERIFICATION_REF_MAX_ITEMS {
            bail!("task participant result has too many verification refs");
        }
        for reference in &self.verification_refs {
            validate_bounded_participant_result_field(
                "verification ref",
                reference,
                TASK_PARTICIPANT_RESULT_REF_MAX_CHARS,
            )?;
        }
        Ok(())
    }
}

fn validate_bounded_participant_result_field(
    field: &str,
    value: &str,
    max_chars: usize,
) -> Result<()> {
    if value.is_empty() {
        bail!("task participant result {field} cannot be empty");
    }
    if value.chars().count() > max_chars || crate::safe_persistence_text(value) != value {
        bail!("task participant result {field} is not safely bounded");
    }
    Ok(())
}

/// Parent commit proving that exactly one synthesis result became the task's visible final answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskFinalAnswerCommittedEntry {
    pub task_id: TaskId,
    pub plan_version: u32,
    pub synthesis_attempt_id: TaskParticipantAttemptId,
    pub message_id: String,
    pub content_hash: String,
}

/// Append-only parent-to-child session link.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskChildSessionEntry {
    pub task_id: TaskId,
    pub plan_version: u32,
    pub step_id: TaskStepId,
    pub child_task_id: TaskId,
    pub child_session_ref: SessionRef,
    pub role: AgentRole,
    pub status: TaskChildSessionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_hash: Option<String>,
}

/// Append-only user-facing display name for a child agent session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskChildSessionDisplayNameEntry {
    pub task_id: TaskId,
    pub plan_version: u32,
    pub step_id: TaskStepId,
    pub child_task_id: TaskId,
    pub display_name: String,
}

/// Append-only parent record for a subagent approval route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskSubagentApprovalRouteEntry {
    pub route_id: TaskRouteId,
    pub task_id: TaskId,
    pub plan_version: u32,
    pub step_id: TaskStepId,
    pub role: AgentRole,
    pub child_session_ref: SessionRef,
    pub call_id: String,
    pub tool_name: String,
    pub status: TaskRouteStatus,
}

/// Append-only parent record for a subagent MCP elicitation route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskSubagentElicitationRouteEntry {
    pub route_id: TaskRouteId,
    pub task_id: TaskId,
    pub plan_version: u32,
    pub step_id: TaskStepId,
    pub role: AgentRole,
    pub child_session_ref: SessionRef,
    pub server_name: String,
    pub status: TaskRouteStatus,
}

/// Materialized task state reconstructed from append-only session entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskStateProjection {
    pub tasks: BTreeMap<TaskId, TaskRunProjection>,
    pub latest_task_id: Option<TaskId>,
    pub task_replay_order: Vec<TaskId>,
}

impl TaskStateProjection {
    /// Replays session entries into the latest task projection.
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

    pub fn latest_task(&self) -> Option<&TaskRunProjection> {
        self.latest_task_id
            .as_ref()
            .and_then(|task_id| self.tasks.get(task_id))
    }

    pub fn latest_unfinished_task(&self) -> Option<&TaskRunProjection> {
        let mut seen = BTreeSet::new();
        self.task_replay_order.iter().rev().find_map(|task_id| {
            if !seen.insert(task_id.clone()) {
                return None;
            }
            self.tasks.get(task_id).filter(|task| {
                !matches!(
                    task.status,
                    TaskRunStatus::Completed | TaskRunStatus::Cancelled
                )
            })
        })
    }

    pub(crate) fn apply_control_entry(&mut self, control: &ControlEntry) {
        match control {
            ControlEntry::TaskRun(entry) => self.apply_run(entry),
            ControlEntry::TaskPlan(entry) => self.apply_plan(entry),
            ControlEntry::TaskStep(entry) => self.apply_step(entry),
            ControlEntry::TaskParticipantAttempt(entry) => self.apply_participant_attempt(entry),
            ControlEntry::TaskParticipantRetryScheduled(entry) => {
                self.apply_participant_retry_scheduled(entry)
            }
            ControlEntry::TaskParticipantResult(entry) => self.apply_participant_result(entry),
            ControlEntry::TaskFinalAnswerCommitted(entry) => self.apply_final_answer(entry),
            ControlEntry::TaskChildSession(entry) => self.apply_child_session(entry),
            ControlEntry::TaskChildSessionDisplayName(entry) => {
                self.apply_child_display_name(entry)
            }
            ControlEntry::TaskSubagentApprovalRoute(entry) => self.apply_approval_route(entry),
            ControlEntry::TaskSubagentElicitationRoute(entry) => {
                self.apply_elicitation_route(entry);
            }
            _ => {}
        }
    }

    fn apply_run(&mut self, entry: &TaskRunEntry) {
        self.record_task_replay(&entry.task_id);
        let task = self
            .tasks
            .entry(entry.task_id.clone())
            .or_insert_with(|| TaskRunProjection::from_run(entry));
        if task.status.is_final() && entry.status != task.status {
            task.duplicate_terminal_entries += usize::from(entry.status.is_terminal());
            return;
        }
        task.objective = entry.objective.clone();
        task.parent_session_ref = entry.parent_session_ref.clone();
        task.status = entry.status;
        task.reason = entry.reason.clone();
        if entry.status.is_terminal() {
            task.active_steps.clear();
            task.current_step = None;
        }
    }

    fn apply_plan(&mut self, entry: &TaskPlanEntry) {
        self.record_task_replay(&entry.task_id);
        let task = self.ensure_task(&entry.task_id);
        if entry.status != TaskPlanStatus::Superseded {
            task.latest_plan_version = Some(entry.plan_version);
        }
        if entry.status == TaskPlanStatus::Accepted {
            let previous_versions = task
                .plans
                .keys()
                .copied()
                .filter(|version| *version != entry.plan_version)
                .collect::<Vec<_>>();
            for version in previous_versions {
                if let Some(plan) = task.plans.get_mut(&version)
                    && plan.status != TaskPlanStatus::Superseded
                {
                    plan.status = TaskPlanStatus::Superseded;
                    task.superseded_plan_versions.insert(version);
                }
                supersede_plan_steps(task, version, entry.plan_version);
            }
        }
        let graph_result = TaskGraphProjection::from_plan_entry(entry);
        let (graph, graph_validation_error) = match graph_result {
            Ok(graph) => (Some(graph), None),
            Err(error) => (None, Some(error.to_string())),
        };
        task.plans.insert(
            entry.plan_version,
            TaskPlanProjection {
                plan_version: entry.plan_version,
                status: entry.status,
                steps: entry.steps.clone(),
                graph,
                graph_validation_error,
                reason: entry.reason.clone(),
            },
        );
    }

    fn apply_step(&mut self, entry: &TaskStepEntry) {
        self.record_task_replay(&entry.task_id);
        let task = self.ensure_task(&entry.task_id);
        let step = task
            .steps
            .entry((entry.plan_version, entry.step_id.clone()))
            .or_insert_with(|| TaskStepProjection::from_step(entry));
        if step.status.is_final() && entry.status != step.status {
            task.duplicate_terminal_entries += usize::from(entry.status.is_terminal());
            return;
        }
        *step = TaskStepProjection::from_step(entry);
        let step_key = (entry.plan_version, entry.step_id.clone());
        if entry.status == TaskStepStatus::Running {
            task.active_steps.insert(step_key);
        } else {
            task.active_steps.remove(&step_key);
        }
        refresh_current_step(task);
    }

    fn apply_participant_attempt(&mut self, entry: &TaskParticipantAttemptEntry) {
        self.record_task_replay(&entry.task_id);
        let task = self.ensure_task(&entry.task_id);
        if entry.validate_shape().is_err() {
            task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            return;
        }
        let attempt = task
            .participant_attempts
            .entry(entry.attempt_id.clone())
            .or_insert_with(|| entry.clone());
        if attempt.task_id != entry.task_id
            || attempt.purpose != entry.purpose
            || attempt.ordinal != entry.ordinal
            || attempt.plan_version != entry.plan_version
            || attempt.step_id != entry.step_id
            || attempt.role != entry.role
            || attempt.child_session_ref != entry.child_session_ref
        {
            task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            return;
        }
        if attempt.status.is_terminal() && attempt.status != entry.status {
            task.duplicate_terminal_entries = task.duplicate_terminal_entries.saturating_add(1);
            return;
        }
        if entry.status.is_terminal()
            && task
                .participant_results
                .get(&entry.attempt_id)
                .and_then(|result| result.terminal_status)
                .is_some_and(|status| status != entry.status)
        {
            task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            return;
        }
        *attempt = entry.clone();
    }

    fn apply_participant_retry_scheduled(&mut self, entry: &TaskParticipantRetryScheduledEntry) {
        self.record_task_replay(&entry.task_id);
        let task = self.ensure_task(&entry.task_id);
        let failed = task.participant_attempts.get(&entry.failed_attempt_id);
        if entry.validate_shape().is_err()
            || failed.is_none_or(|attempt| {
                attempt.task_id != entry.task_id
                    || attempt.purpose != entry.purpose
                    || attempt.plan_version != entry.plan_version
                    || attempt.step_id != entry.step_id
                    || attempt.ordinal.saturating_add(1) != entry.retry_ordinal
                    || attempt.status != TaskParticipantAttemptStatus::Failed
            })
            || task
                .participant_attempts
                .get(&entry.retry_attempt_id)
                .is_some_and(|attempt| attempt.ordinal != entry.retry_ordinal)
        {
            task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            return;
        }
        match task
            .participant_retry_schedules
            .get(&entry.retry_attempt_id)
        {
            Some(existing) if existing != entry => {
                task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            }
            Some(_) => {}
            None => {
                task.participant_retry_schedules
                    .insert(entry.retry_attempt_id.clone(), entry.clone());
            }
        }
    }

    fn apply_participant_result(&mut self, entry: &TaskParticipantResultEntry) {
        self.record_task_replay(&entry.task_id);
        let task = self.ensure_task(&entry.task_id);
        let attempt = task.participant_attempts.get(&entry.attempt_id);
        if entry.validate_shape().is_err()
            || attempt.is_none_or(|attempt| attempt.task_id != entry.task_id)
            || entry.terminal_status.is_some_and(|status| {
                attempt
                    .is_some_and(|attempt| attempt.status.is_terminal() && attempt.status != status)
            })
            || entry.final_answer_ref.as_ref().is_some_and(|reference| {
                attempt.is_none_or(|attempt| {
                    reference.session_ref != attempt.child_session_ref
                        || format!("sha256:{}", reference.content_hash) != entry.output_hash
                })
            })
        {
            task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            return;
        }
        match task.participant_results.get(&entry.attempt_id) {
            Some(existing) if existing != entry => {
                task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            }
            Some(_) => {}
            None => {
                task.participant_results
                    .insert(entry.attempt_id.clone(), entry.clone());
            }
        }
    }

    fn apply_final_answer(&mut self, entry: &TaskFinalAnswerCommittedEntry) {
        self.record_task_replay(&entry.task_id);
        let task = self.ensure_task(&entry.task_id);
        if task
            .participant_attempts
            .get(&entry.synthesis_attempt_id)
            .is_none_or(|attempt| {
                attempt.purpose != TaskParticipantPurpose::Synthesis
                    || attempt.plan_version != Some(entry.plan_version)
                    || attempt.status != TaskParticipantAttemptStatus::Completed
            })
            || task
                .participant_results
                .get(&entry.synthesis_attempt_id)
                .is_none_or(|result| result.output_hash != entry.content_hash)
            || entry.message_id
                != task_final_message_id(&entry.task_id, &entry.synthesis_attempt_id)
        {
            task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            return;
        }
        match &task.final_answer {
            Some(existing) if existing != entry => {
                task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            }
            Some(_) => {}
            None => task.final_answer = Some(entry.clone()),
        }
    }

    fn apply_child_session(&mut self, entry: &TaskChildSessionEntry) {
        self.record_task_replay(&entry.task_id);
        let task = self.ensure_task(&entry.task_id);
        if entry.status == TaskChildSessionStatus::Unavailable {
            task.child_unavailable = true;
        }
        task.child_sessions.insert(
            (
                entry.plan_version,
                entry.step_id.clone(),
                entry.child_task_id.clone(),
            ),
            entry.clone(),
        );
    }

    fn apply_child_display_name(&mut self, entry: &TaskChildSessionDisplayNameEntry) {
        self.record_task_replay(&entry.task_id);
        let task = self.ensure_task(&entry.task_id);
        if let Ok(display_name) = normalize_task_agent_display_name(&entry.display_name) {
            task.child_display_names.insert(
                child_session_projection_key(
                    entry.plan_version,
                    &entry.step_id,
                    &entry.child_task_id,
                ),
                display_name,
            );
        }
    }

    fn apply_approval_route(&mut self, entry: &TaskSubagentApprovalRouteEntry) {
        self.record_task_replay(&entry.task_id);
        let task = self.ensure_task(&entry.task_id);
        let child_matches = task.child_sessions.values().any(|child| {
            child.plan_version == entry.plan_version
                && child.step_id == entry.step_id
                && child.child_session_ref == entry.child_session_ref
        });
        if !child_matches {
            task.route_unverified = true;
        }
        task.approval_routes
            .insert(entry.route_id.clone(), entry.clone());
    }

    fn apply_elicitation_route(&mut self, entry: &TaskSubagentElicitationRouteEntry) {
        self.record_task_replay(&entry.task_id);
        let task = self.ensure_task(&entry.task_id);
        let child_matches = task.child_sessions.values().any(|child| {
            child.plan_version == entry.plan_version
                && child.step_id == entry.step_id
                && child.child_session_ref == entry.child_session_ref
        });
        if !child_matches {
            task.route_unverified = true;
        }
        task.elicitation_routes
            .insert(entry.route_id.clone(), entry.clone());
    }

    fn ensure_task(&mut self, task_id: &TaskId) -> &mut TaskRunProjection {
        self.tasks
            .entry(task_id.clone())
            .or_insert_with(|| TaskRunProjection::placeholder(task_id.clone()))
    }

    fn record_task_replay(&mut self, task_id: &TaskId) {
        self.latest_task_id = Some(task_id.clone());
        self.task_replay_order.push(task_id.clone());
    }
}

/// Projection for one task run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRunProjection {
    pub task_id: TaskId,
    pub parent_session_ref: SessionRef,
    pub objective: String,
    pub status: TaskRunStatus,
    pub reason: Option<String>,
    pub latest_plan_version: Option<u32>,
    pub plans: BTreeMap<u32, TaskPlanProjection>,
    pub steps: BTreeMap<(u32, TaskStepId), TaskStepProjection>,
    /// All task steps whose latest append-only status is `Running`.
    pub active_steps: BTreeSet<(u32, TaskStepId)>,
    /// Compatibility view populated only when exactly one task step is active.
    pub current_step: Option<(u32, TaskStepId)>,
    pub participant_attempts: BTreeMap<TaskParticipantAttemptId, TaskParticipantAttemptEntry>,
    pub participant_retry_schedules:
        BTreeMap<TaskParticipantAttemptId, TaskParticipantRetryScheduledEntry>,
    pub participant_results: BTreeMap<TaskParticipantAttemptId, TaskParticipantResultEntry>,
    pub final_answer: Option<TaskFinalAnswerCommittedEntry>,
    pub child_sessions: BTreeMap<(u32, TaskStepId, TaskId), TaskChildSessionEntry>,
    pub child_display_names: BTreeMap<(u32, TaskStepId, TaskId), String>,
    pub approval_routes: BTreeMap<TaskRouteId, TaskSubagentApprovalRouteEntry>,
    pub elicitation_routes: BTreeMap<TaskRouteId, TaskSubagentElicitationRouteEntry>,
    pub duplicate_terminal_entries: usize,
    pub superseded_plan_versions: BTreeSet<u32>,
    pub route_unverified: bool,
    pub child_unavailable: bool,
    pub participant_conflicts: usize,
}

impl TaskRunProjection {
    fn from_run(entry: &TaskRunEntry) -> Self {
        Self {
            task_id: entry.task_id.clone(),
            parent_session_ref: entry.parent_session_ref.clone(),
            objective: entry.objective.clone(),
            status: entry.status,
            reason: entry.reason.clone(),
            latest_plan_version: None,
            plans: BTreeMap::new(),
            steps: BTreeMap::new(),
            active_steps: BTreeSet::new(),
            current_step: None,
            participant_attempts: BTreeMap::new(),
            participant_retry_schedules: BTreeMap::new(),
            participant_results: BTreeMap::new(),
            final_answer: None,
            child_sessions: BTreeMap::new(),
            child_display_names: BTreeMap::new(),
            approval_routes: BTreeMap::new(),
            elicitation_routes: BTreeMap::new(),
            duplicate_terminal_entries: 0,
            superseded_plan_versions: BTreeSet::new(),
            route_unverified: false,
            child_unavailable: false,
            participant_conflicts: 0,
        }
    }

    fn placeholder(task_id: TaskId) -> Self {
        Self {
            task_id,
            parent_session_ref: SessionRef {
                path: "unknown.jsonl".to_owned(),
            },
            objective: String::new(),
            status: TaskRunStatus::Started,
            reason: None,
            latest_plan_version: None,
            plans: BTreeMap::new(),
            steps: BTreeMap::new(),
            active_steps: BTreeSet::new(),
            current_step: None,
            participant_attempts: BTreeMap::new(),
            participant_retry_schedules: BTreeMap::new(),
            participant_results: BTreeMap::new(),
            final_answer: None,
            child_sessions: BTreeMap::new(),
            child_display_names: BTreeMap::new(),
            approval_routes: BTreeMap::new(),
            elicitation_routes: BTreeMap::new(),
            duplicate_terminal_entries: 0,
            superseded_plan_versions: BTreeSet::new(),
            route_unverified: false,
            child_unavailable: false,
            participant_conflicts: 0,
        }
    }

    /// Returns participant attempts for one purpose in durable ordinal order.
    pub fn participant_attempts_for(
        &self,
        purpose: TaskParticipantPurpose,
        plan_version: Option<u32>,
        step_id: Option<&TaskStepId>,
    ) -> Vec<&TaskParticipantAttemptEntry> {
        let mut attempts = self
            .participant_attempts
            .values()
            .filter(|attempt| {
                attempt.purpose == purpose
                    && attempt.plan_version == plan_version
                    && attempt.step_id.as_ref() == step_id
            })
            .collect::<Vec<_>>();
        attempts.sort_by_key(|attempt| attempt.ordinal);
        attempts
    }

    /// Returns the next retry ordinal for one participant identity.
    #[must_use]
    pub fn next_participant_ordinal(
        &self,
        purpose: TaskParticipantPurpose,
        plan_version: Option<u32>,
        step_id: Option<&TaskStepId>,
    ) -> u32 {
        self.participant_attempts_for(purpose, plan_version, step_id)
            .into_iter()
            .map(|attempt| attempt.ordinal)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    /// Returns the durable schedule that authorizes the next not-yet-started retry.
    pub fn pending_participant_retry(
        &self,
        purpose: TaskParticipantPurpose,
        plan_version: Option<u32>,
        step_id: Option<&TaskStepId>,
    ) -> Option<&TaskParticipantRetryScheduledEntry> {
        self.participant_retry_schedules
            .values()
            .filter(|schedule| {
                schedule.purpose == purpose
                    && schedule.plan_version == plan_version
                    && schedule.step_id.as_ref() == step_id
                    && !self
                        .participant_attempts
                        .contains_key(&schedule.retry_attempt_id)
            })
            .max_by_key(|schedule| schedule.retry_ordinal)
    }

    /// Returns the cumulative durable retry delay for one participant identity.
    pub fn participant_retry_wait_ms(
        &self,
        purpose: TaskParticipantPurpose,
        plan_version: Option<u32>,
        step_id: Option<&TaskStepId>,
    ) -> u64 {
        self.participant_retry_schedules
            .values()
            .filter(|schedule| {
                schedule.purpose == purpose
                    && schedule.plan_version == plan_version
                    && schedule.step_id.as_ref() == step_id
            })
            .fold(0_u64, |total, schedule| {
                total.saturating_add(schedule.retry_after_ms)
            })
    }

    /// Returns the latest persisted display name for a child session, if one was recorded.
    pub fn display_name_for_child_session(&self, child: &TaskChildSessionEntry) -> Option<&str> {
        self.child_display_names
            .get(&child_session_projection_key(
                child.plan_version,
                &child.step_id,
                &child.child_task_id,
            ))
            .map(String::as_str)
    }
}

fn supersede_plan_steps(
    task: &mut TaskRunProjection,
    old_plan_version: u32,
    new_plan_version: u32,
) {
    let Some(plan) = task.plans.get(&old_plan_version) else {
        return;
    };
    let steps = plan.steps.clone();
    for step in steps {
        let key = (old_plan_version, step.step_id.clone());
        if task
            .steps
            .get(&key)
            .is_some_and(|projection| projection.status == TaskStepStatus::Completed)
        {
            continue;
        }
        task.steps.insert(
            key,
            TaskStepProjection {
                task_id: task.task_id.clone(),
                plan_version: old_plan_version,
                step_id: step.step_id,
                role: step.role,
                status: TaskStepStatus::Superseded,
                title: Some(step.title),
                summary: None,
                reason: Some(format!("superseded by accepted plan v{new_plan_version}")),
            },
        );
    }
    task.active_steps
        .retain(|(plan_version, _)| *plan_version != old_plan_version);
    refresh_current_step(task);
}

fn refresh_current_step(task: &mut TaskRunProjection) {
    task.current_step = if task.active_steps.len() == 1 {
        task.active_steps.first().cloned()
    } else {
        None
    };
}

/// Projection for one plan version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPlanProjection {
    pub plan_version: u32,
    pub status: TaskPlanStatus,
    pub steps: Vec<TaskStepSpec>,
    pub graph: Option<TaskGraphProjection>,
    pub graph_validation_error: Option<String>,
    pub reason: Option<String>,
}

/// Durable DAG view reconstructed from a task plan entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskGraphProjection {
    pub task_id: TaskId,
    pub graph_version: u32,
    pub steps: Vec<TaskGraphStepProjection>,
}

impl TaskGraphProjection {
    /// Builds a graph projection from one accepted or proposed task plan.
    ///
    /// # Errors
    ///
    /// Returns an error when the plan carries invalid DAG metadata.
    pub fn from_plan_entry(entry: &TaskPlanEntry) -> Result<Self> {
        validate_task_plan_graph_steps(&entry.steps)?;
        Ok(Self {
            task_id: entry.task_id.clone(),
            graph_version: entry.plan_version,
            steps: entry
                .steps
                .iter()
                .map(TaskGraphStepProjection::from_step_spec)
                .collect(),
        })
    }

    pub fn ready_steps<'a>(
        &'a self,
        statuses: &'a BTreeMap<(u32, TaskStepId), TaskStepProjection>,
    ) -> Vec<&'a TaskGraphStepProjection> {
        self.steps
            .iter()
            .filter(|step| {
                let step_key = (self.graph_version, step.step_id.clone());
                let not_started = statuses.get(&step_key).is_none_or(|status| {
                    matches!(
                        status.status,
                        TaskStepStatus::Pending | TaskStepStatus::Interrupted
                    )
                });
                not_started
                    && step.depends_on.iter().all(|dependency| {
                        statuses
                            .get(&(self.graph_version, dependency.clone()))
                            .is_some_and(|status| status.status == TaskStepStatus::Completed)
                    })
            })
            .collect()
    }

    #[must_use]
    pub fn ready_queue(
        &self,
        statuses: &BTreeMap<(u32, TaskStepId), TaskStepProjection>,
        options: TaskReadyQueueOptions,
    ) -> TaskReadyQueue {
        self.ready_queue_with_active_write_lease(statuses, options, false)
    }

    #[must_use]
    pub fn ready_queue_with_active_write_lease(
        &self,
        statuses: &BTreeMap<(u32, TaskStepId), TaskStepProjection>,
        options: TaskReadyQueueOptions,
        active_write_lease: bool,
    ) -> TaskReadyQueue {
        let ready_steps = self.ready_steps(statuses);
        if active_write_lease {
            return TaskReadyQueue {
                read_only_batch: Vec::new(),
                sequential_step: None,
                deferred: ready_steps
                    .into_iter()
                    .map(|step| TaskReadyDeferredStep {
                        step_id: step.step_id.clone(),
                        reason: TaskReadyDeferredReason::ActiveWriteLease,
                    })
                    .collect(),
            };
        }
        let running_steps = self.running_steps(statuses);
        let running_write = running_steps
            .iter()
            .any(|step| !step.is_parallel_read_only());
        if running_write {
            return TaskReadyQueue {
                read_only_batch: Vec::new(),
                sequential_step: None,
                deferred: ready_steps
                    .into_iter()
                    .map(|step| TaskReadyDeferredStep {
                        step_id: step.step_id.clone(),
                        reason: TaskReadyDeferredReason::RunningWrite,
                    })
                    .collect(),
            };
        }

        let running_read_only = running_steps
            .iter()
            .filter(|step| step.is_parallel_read_only())
            .count();
        let read_only_capacity = options
            .max_concurrent_read_only
            .saturating_sub(running_read_only);
        let mut read_only_batch = Vec::new();
        let mut sequential_step = None;
        let mut deferred = Vec::new();
        let mut ready_write_steps = Vec::new();

        for step in ready_steps {
            if step.is_parallel_read_only() {
                if read_only_batch.len() < read_only_capacity {
                    read_only_batch.push(step.clone());
                } else {
                    deferred.push(TaskReadyDeferredStep {
                        step_id: step.step_id.clone(),
                        reason: TaskReadyDeferredReason::ConcurrencyBudget,
                    });
                }
            } else {
                ready_write_steps.push(step);
            }
        }

        let may_start_write = read_only_batch.is_empty() && running_read_only == 0;
        if may_start_write {
            sequential_step = ready_write_steps.first().map(|step| (*step).clone());
        }
        for (index, step) in ready_write_steps.into_iter().enumerate() {
            if may_start_write && index == 0 {
                continue;
            }
            deferred.push(TaskReadyDeferredStep {
                step_id: step.step_id.clone(),
                reason: if running_read_only > 0 {
                    TaskReadyDeferredReason::RunningReadOnly
                } else {
                    TaskReadyDeferredReason::SequentialWrite
                },
            });
        }

        TaskReadyQueue {
            read_only_batch,
            sequential_step,
            deferred,
        }
    }

    fn running_steps<'a>(
        &'a self,
        statuses: &'a BTreeMap<(u32, TaskStepId), TaskStepProjection>,
    ) -> Vec<&'a TaskGraphStepProjection> {
        self.steps
            .iter()
            .filter(|step| {
                statuses
                    .get(&(self.graph_version, step.step_id.clone()))
                    .is_some_and(|status| status.status == TaskStepStatus::Running)
            })
            .collect()
    }
}

/// One task graph step as materialized for scheduling and TUI summaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskGraphStepProjection {
    pub step_id: TaskStepId,
    pub title: String,
    pub mode: TaskStepMode,
    pub depends_on: Vec<TaskStepId>,
    pub isolation: TaskIsolationMode,
}

impl TaskGraphStepProjection {
    fn from_step_spec(step: &TaskStepSpec) -> Self {
        Self {
            step_id: step.step_id.clone(),
            title: step.title.clone(),
            mode: step.effective_mode(),
            depends_on: step.depends_on.clone(),
            isolation: step.effective_isolation(),
        }
    }

    #[must_use]
    pub fn is_parallel_read_only(&self) -> bool {
        matches!(
            self.mode,
            TaskStepMode::Read | TaskStepMode::Review | TaskStepMode::Verify
        ) && self.isolation == TaskIsolationMode::SharedReadOnly
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskReadyQueueOptions {
    pub max_concurrent_read_only: usize,
}

impl TaskReadyQueueOptions {
    #[must_use]
    pub fn new(max_concurrent_read_only: usize) -> Self {
        Self {
            max_concurrent_read_only,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReadyQueue {
    pub read_only_batch: Vec<TaskGraphStepProjection>,
    pub sequential_step: Option<TaskGraphStepProjection>,
    pub deferred: Vec<TaskReadyDeferredStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReadyDeferredStep {
    pub step_id: TaskStepId,
    pub reason: TaskReadyDeferredReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskReadyDeferredReason {
    ActiveWriteLease,
    ConcurrencyBudget,
    RunningReadOnly,
    RunningWrite,
    SequentialWrite,
}

/// Projection for one task step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStepProjection {
    pub task_id: TaskId,
    pub plan_version: u32,
    pub step_id: TaskStepId,
    pub role: AgentRole,
    pub status: TaskStepStatus,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub reason: Option<String>,
}

impl TaskStepProjection {
    fn from_step(entry: &TaskStepEntry) -> Self {
        Self {
            task_id: entry.task_id.clone(),
            plan_version: entry.plan_version,
            step_id: entry.step_id.clone(),
            role: entry.role,
            status: entry.status,
            title: entry.title.clone(),
            summary: entry.summary.clone(),
            reason: entry.reason.clone(),
        }
    }
}

fn validate_stable_id(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{label} cannot be empty");
    }
    if value.len() > 96 {
        bail!("{label} is too long");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("{label} contains unsupported characters");
    }
    Ok(())
}

fn validate_sha256_fingerprint(label: &str, value: &str) -> Result<()> {
    validate_prefixed_sha256(label, value, "sha256:")
}

fn validate_prefixed_sha256(label: &str, value: &str, prefix: &str) -> Result<()> {
    let Some(digest) = value.strip_prefix(prefix) else {
        bail!("{label} must use a {prefix} fingerprint");
    };
    validate_hex_sha256(label, digest)
}

fn validate_hex_sha256(label: &str, value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} must contain a 64-character hexadecimal sha256 digest");
    }
    Ok(())
}

fn task_domain_hash(domain: &str, parts: &[&str]) -> String {
    let mut digest = sha2::Sha256::new();
    digest.update(domain.as_bytes());
    for part in parts {
        digest.update([0]);
        digest.update(part.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn task_text_hash(value: &str) -> String {
    let mut digest = sha2::Sha256::new();
    digest.update(value.as_bytes());
    format!("{:x}", digest.finalize())
}

/// Normalizes and validates a user-facing task agent display name.
///
/// # Errors
///
/// Returns an error when the name is empty after trimming, too long, or contains control
/// characters that would make persisted TUI state hard to render safely.
pub fn normalize_task_agent_display_name(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("agent display name cannot be empty");
    }
    if value.chars().count() > TASK_AGENT_DISPLAY_NAME_MAX_CHARS {
        bail!("agent display name is too long");
    }
    if value.chars().any(char::is_control) {
        bail!("agent display name contains control characters");
    }
    Ok(value.to_owned())
}

fn child_session_projection_key(
    plan_version: u32,
    step_id: &TaskStepId,
    child_task_id: &TaskId,
) -> (u32, TaskStepId, TaskId) {
    (plan_version, step_id.clone(), child_task_id.clone())
}

fn validate_relative_session_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        bail!("session reference cannot be empty");
    }
    if path.is_absolute() {
        bail!("session reference must be relative");
    }
    let mut has_component = false;
    for component in path.components() {
        match component {
            Component::Normal(_) => has_component = true,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!("session reference cannot escape session directory"));
            }
        }
    }
    if !has_component {
        bail!("session reference must contain a file path");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/task_tests.rs"]
mod tests;
