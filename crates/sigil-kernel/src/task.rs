use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    provider::ToolCall,
    session::{ControlEntry, SessionLogEntry},
    tool::{ToolAccess, ToolCategory, ToolPreviewCapability, ToolSpec},
};

pub const TASK_PLAN_UPDATE_TOOL_NAME: &str = "task_plan_update";

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
}

impl TaskStepStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Blocked | Self::Cancelled | Self::Interrupted
        )
    }

    fn is_final(self) -> bool {
        matches!(self, Self::Completed)
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub role: AgentRole,
}

/// Bound task context for the internal planner tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskPlanUpdateContext {
    pub task_id: TaskId,
    pub max_plan_steps: usize,
}

/// Model-visible schema for the internal planner plan-update tool.
pub fn task_plan_update_tool_spec() -> ToolSpec {
    ToolSpec {
        name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
        description:
            "Create or replace the current durable task plan. Use this before executing task steps."
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
                            "detail": {"type": "string"},
                            "role": {
                                "type": "string",
                                "enum": ["planner", "executor", "subagent_read", "subagent_write"]
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
            Ok(TaskStepSpec {
                step_id: TaskStepId::new(step.step_id)?,
                title: step.title,
                detail: step.detail,
                role: step.role,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(TaskPlanEntry {
        task_id: context.task_id.clone(),
        plan_version: args.plan_version,
        status: args.status,
        steps,
        reason: args.reason,
    })
}

/// Bounded model-visible response content for `task_plan_update`.
pub fn task_plan_update_result_content(entry: &TaskPlanEntry) -> String {
    json!({
        "task_id": entry.task_id.as_str(),
        "plan_version": entry.plan_version,
        "status": entry.status,
        "steps": entry.steps.len()
    })
    .to_string()
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
    pub detail: Option<String>,
    pub role: AgentRole,
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
            projection.apply_control(control);
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

    fn apply_control(&mut self, control: &ControlEntry) {
        match control {
            ControlEntry::TaskRun(entry) => self.apply_run(entry),
            ControlEntry::TaskPlan(entry) => self.apply_plan(entry),
            ControlEntry::TaskStep(entry) => self.apply_step(entry),
            ControlEntry::TaskChildSession(entry) => self.apply_child_session(entry),
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
            }
        }
        task.plans.insert(
            entry.plan_version,
            TaskPlanProjection {
                plan_version: entry.plan_version,
                status: entry.status,
                steps: entry.steps.clone(),
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
        if entry.status == TaskStepStatus::Running {
            task.current_step = Some((entry.plan_version, entry.step_id.clone()));
        } else if task
            .current_step
            .as_ref()
            .is_some_and(|current| current == &(entry.plan_version, entry.step_id.clone()))
        {
            task.current_step = None;
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
    pub current_step: Option<(u32, TaskStepId)>,
    pub child_sessions: BTreeMap<(u32, TaskStepId, TaskId), TaskChildSessionEntry>,
    pub approval_routes: BTreeMap<TaskRouteId, TaskSubagentApprovalRouteEntry>,
    pub elicitation_routes: BTreeMap<TaskRouteId, TaskSubagentElicitationRouteEntry>,
    pub duplicate_terminal_entries: usize,
    pub superseded_plan_versions: BTreeSet<u32>,
    pub route_unverified: bool,
    pub child_unavailable: bool,
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
            current_step: None,
            child_sessions: BTreeMap::new(),
            approval_routes: BTreeMap::new(),
            elicitation_routes: BTreeMap::new(),
            duplicate_terminal_entries: 0,
            superseded_plan_versions: BTreeSet::new(),
            route_unverified: false,
            child_unavailable: false,
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
            current_step: None,
            child_sessions: BTreeMap::new(),
            approval_routes: BTreeMap::new(),
            elicitation_routes: BTreeMap::new(),
            duplicate_terminal_entries: 0,
            superseded_plan_versions: BTreeSet::new(),
            route_unverified: false,
            child_unavailable: false,
        }
    }
}

/// Projection for one plan version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPlanProjection {
    pub plan_version: u32,
    pub status: TaskPlanStatus,
    pub steps: Vec<TaskStepSpec>,
    pub reason: Option<String>,
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
