use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Component, Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::{
    AgentArtifactRef, AgentFinalAnswerRef, AgentThreadId,
    session::{ControlEntry, SessionLogEntry},
};

/// Durable schema carried by task-step execution-contract sidecars.
pub const TASK_STEP_CONTRACT_V2_SCHEMA_VERSION: u16 = 2;
const TASK_STEP_CONTRACT_MAX_ITEMS: usize = 64;
const TASK_STEP_CONTRACT_MAX_TEXT_CHARS: usize = 2_048;
const TASK_STEP_CONTRACT_MAX_PATH_CHARS: usize = 1_024;
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
const TASK_PARTICIPANT_ATTEMPT_ID_DOMAIN: &str = "sigil-task-participant-attempt-v1";
const TASK_RUN_TARGET_SELECTION_DOMAIN: &str = "sigil-task-run-target-selection-v1";

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

/// Stable identifier for one task-step participant attempt.
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

/// Participant phase owned by one isolated transcript.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaskParticipantPurpose {
    Step,
}

impl TaskParticipantPurpose {
    pub fn as_str(self) -> &'static str {
        "step"
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

/// Lifecycle of one direct Task execution attempt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskExecutionAttemptStatus {
    Started,
    Completed,
    Failed,
    Blocked,
    Cancelled,
    Interrupted,
}

impl TaskExecutionAttemptStatus {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        self != Self::Started
    }
}

/// Builds the stable identity for one task participant attempt.
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

/// Produces the bounded, persistence-safe result summary stored in the parent control log.
#[must_use]
pub fn bounded_task_participant_summary(value: &str) -> String {
    let bounded = crate::safe_persistence_text(value)
        .trim()
        .chars()
        .take(TASK_PARTICIPANT_RESULT_SUMMARY_MAX_CHARS)
        .collect::<String>();
    bounded.trim_end().to_owned()
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
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

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

/// User-facing phase projected from the current Direct Task lifecycle.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskExecutionPhaseV1 {
    Ready,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl TaskExecutionPhaseV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

/// A terminal fact that is about to replace one currently-started participant during root
/// completion evaluation.
///
/// The evaluator consumes this only as an in-memory candidate. Callers must still append the
/// matching durable attempt terminal before they append a Task root terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskRootTerminalCandidateV1 {
    DirectExecution {
        attempt_id: String,
        status: TaskExecutionAttemptStatus,
    },
    Participant {
        attempt_id: TaskParticipantAttemptId,
        status: TaskParticipantAttemptStatus,
    },
}

/// Typed reason why a requested Task root completion cannot be accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskRootCompletionBlockerV1 {
    FailedDependency,
    BlockedDependency,
    CancelledDependency,
    UnfinishedStep,
    UnfinishedParticipant,
    UnfinishedDirectExecution,
    UnfinishedBackgroundAgent,
}

impl TaskRootCompletionBlockerV1 {
    #[must_use]
    pub fn reason_code(self) -> &'static str {
        match self {
            Self::FailedDependency => "failed_dependency",
            Self::BlockedDependency => "blocked_dependency",
            Self::CancelledDependency => "cancelled_dependency",
            Self::UnfinishedStep => "unfinished_task_step",
            Self::UnfinishedParticipant => "unfinished_task_participant",
            Self::UnfinishedDirectExecution => "unfinished_direct_execution",
            Self::UnfinishedBackgroundAgent => "unfinished_direct_task_background_agent",
        }
    }
}

/// Durable-only evaluation of a proposed Task root terminal.
///
/// `Completed` is accepted only when the selected direct execution or IntentStack TaskPlan has
/// no unfinished participant or step. For a cancelled root, the evaluator
/// also returns the complete set of non-completed current-plan steps that must receive a
/// cancellation terminal before the root closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRootTerminalEvaluationV1 {
    pub requested_status: TaskRunStatus,
    pub effective_status: TaskRunStatus,
    /// Descendants of failed, blocked, or interrupted dependencies that cannot continue.
    pub blocked_dependency_steps: Vec<TaskStepId>,
    /// Descendants of a cancelled dependency that must not remain runnable.
    pub cancelled_dependency_steps: Vec<TaskStepId>,
    /// Every current-plan step that a root cancellation must close.
    pub cancellation_closure: Vec<TaskStepId>,
    pub unfinished_steps: Vec<TaskStepId>,
    pub unfinished_participants: Vec<TaskParticipantAttemptId>,
    pub unfinished_direct_attempts: Vec<String>,
    /// Direct Task background agents whose exact durable grant is still active.
    pub unfinished_direct_task_background_agents: Vec<AgentThreadId>,
    pub completion_blockers: Vec<TaskRootCompletionBlockerV1>,
}

impl TaskRootTerminalEvaluationV1 {
    #[must_use]
    pub fn allows_completed(&self) -> bool {
        self.requested_status == TaskRunStatus::Completed
            && self.effective_status == TaskRunStatus::Completed
    }

    #[must_use]
    pub fn primary_completion_blocker(&self) -> Option<TaskRootCompletionBlockerV1> {
        self.completion_blockers.first().copied()
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

impl TaskPlanStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Accepted => "accepted",
            Self::Superseded => "superseded",
            Self::Rejected => "rejected",
        }
    }
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
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Superseded => "superseded",
        }
    }

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
    Blocked,
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
    Expired,
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
    /// Runtime-resolved accepted Intent versions served by this step.
    ///
    /// Provider-authored aliases must be resolved by the host before the TaskPlan is accepted.
    /// Write steps participating in Intent Stack V1 must bind exactly one ref; read/review steps
    /// may bind an accepted dependency closure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intent_refs: Vec<crate::IntentVersionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<TaskStepMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<TaskIsolationMode>,
}

/// Runtime-derived contiguous execution unit; each member retains its own durable lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskExecutionSegmentV1 {
    pub step_ids: Vec<TaskStepId>,
    pub role: AgentRole,
    pub mode: TaskStepMode,
    pub isolation: TaskIsolationMode,
}

/// Groups only exact linear neighbours with identical execution contracts.
#[must_use]
pub fn derive_task_execution_segments(steps: &[TaskStepSpec]) -> Vec<TaskExecutionSegmentV1> {
    let mut segments = Vec::new();
    for step in steps {
        let mode = step.mode.unwrap_or(TaskStepMode::Read);
        let isolation = step
            .isolation
            .unwrap_or_else(|| TaskIsolationMode::default_for_mode(mode));
        let joins_previous = segments
            .last()
            .is_some_and(|segment: &TaskExecutionSegmentV1| {
                let Some(previous) = segment.step_ids.last() else {
                    return false;
                };
                step.depends_on.as_slice() == std::slice::from_ref(previous)
                    && segment.role == step.role
                    && segment.mode == mode
                    && segment.isolation == isolation
            });
        if joins_previous {
            if let Some(segment) = segments.last_mut() {
                segment.step_ids.push(step.step_id.clone());
            }
        } else {
            segments.push(TaskExecutionSegmentV1 {
                step_ids: vec![step.step_id.clone()],
                role: step.role,
                mode,
                isolation,
            });
        }
    }
    segments
}

/// Capability a task step must possess before a participant may be launched.
///
/// These values describe semantic abilities, not concrete tool names. The runtime resolves them
/// against the exact scoped registry for the selected participant and fails admission closed when
/// a required capability is missing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaskCapabilityV2 {
    WorkspaceRead,
    WorkspaceWrite,
    VcsRead,
    ProcessExecute,
    NetworkRead,
    ArtifactRead,
    VerificationRun,
}

impl TaskCapabilityV2 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceRead => "workspace_read",
            Self::WorkspaceWrite => "workspace_write",
            Self::VcsRead => "vcs_read",
            Self::ProcessExecute => "process_execute",
            Self::NetworkRead => "network_read",
            Self::ArtifactRead => "artifact_read",
            Self::VerificationRun => "verification_run",
        }
    }

    pub fn tool_capability(self) -> crate::ToolCapability {
        match self {
            Self::WorkspaceRead => crate::ToolCapability::WorkspaceRead,
            Self::WorkspaceWrite => crate::ToolCapability::WorkspaceWrite,
            Self::VcsRead => crate::ToolCapability::VcsRead,
            Self::ProcessExecute => crate::ToolCapability::ProcessExecute,
            Self::NetworkRead => crate::ToolCapability::NetworkRead,
            Self::ArtifactRead => crate::ToolCapability::ArtifactRead,
            Self::VerificationRun => crate::ToolCapability::VerificationRun,
        }
    }
}

/// Versioned, append-only execution contract for one accepted task-plan step.
///
/// This is intentionally a sidecar instead of a field on [`TaskStepSpec`]. Historic V1 plan
/// payloads therefore retain their exact meaning and replay with an empty contract, while V2
/// planners can preserve scope, deliverables, acceptance criteria, and capability requirements.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskStepContractV2 {
    pub schema_version: u16,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_capabilities: Vec<TaskCapabilityV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deliverables: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_spec_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl TaskStepContractV2 {
    /// Validates that the contract is bounded, persistence-safe, and deterministic.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported schema versions, duplicate capabilities or references,
    /// unsafe paths, or unbounded text.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != TASK_STEP_CONTRACT_V2_SCHEMA_VERSION {
            bail!("unsupported task step contract schema version");
        }
        validate_contract_item_count("target paths", self.target_paths.len())?;
        validate_contract_item_count("required capabilities", self.required_capabilities.len())?;
        validate_contract_item_count("deliverables", self.deliverables.len())?;
        validate_contract_item_count("acceptance criteria", self.acceptance_criteria.len())?;
        validate_contract_item_count("check spec refs", self.check_spec_refs.len())?;
        validate_contract_item_count("notes", self.notes.len())?;
        if self
            .required_capabilities
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != self.required_capabilities.len()
        {
            bail!("task step contract repeats a required capability");
        }
        if self.check_spec_refs.iter().collect::<BTreeSet<_>>().len() != self.check_spec_refs.len()
        {
            bail!("task step contract repeats a check spec ref");
        }
        for path in &self.target_paths {
            validate_contract_workspace_path(path)?;
        }
        for (label, values) in [
            ("deliverable", &self.deliverables),
            ("acceptance criterion", &self.acceptance_criteria),
            ("check spec ref", &self.check_spec_refs),
            ("note", &self.notes),
        ] {
            for value in values {
                validate_contract_text(label, value, TASK_STEP_CONTRACT_MAX_TEXT_CHARS)?;
            }
        }
        if let Some(risk) = self.risk.as_deref() {
            validate_contract_text("risk", risk, TASK_STEP_CONTRACT_MAX_TEXT_CHARS)?;
        }
        Ok(())
    }
}

/// Binds a V2 execution contract to one immutable task-plan incarnation and step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskStepContractBoundEntryV2 {
    pub task_id: TaskId,
    pub plan_version: u32,
    pub step_id: TaskStepId,
    pub contract: TaskStepContractV2,
}

/// Terminal marker proving that one accepted plan and its complete V2 sidecar set were committed
/// as a single recovery unit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskPlanContractSetCommittedV2 {
    pub schema_version: u16,
    pub task_id: TaskId,
    pub plan_version: u32,
    pub contract_count: usize,
    pub contract_set_sha256: String,
}

impl TaskPlanContractSetCommittedV2 {
    /// Builds a deterministic commit marker for the exact plan incarnation.
    pub fn new(plan: &TaskPlanEntry, contracts: &[TaskStepContractBoundEntryV2]) -> Result<Self> {
        if contracts.len() != plan.steps.len() {
            bail!("V2 task plan contract set is incomplete");
        }
        let plan_step_ids = plan
            .steps
            .iter()
            .map(|step| &step.step_id)
            .collect::<BTreeSet<_>>();
        let contract_step_ids = contracts
            .iter()
            .map(|binding| &binding.step_id)
            .collect::<BTreeSet<_>>();
        if plan_step_ids != contract_step_ids {
            bail!("V2 task plan contract set does not match plan steps");
        }
        for binding in contracts {
            binding.validate()?;
            if binding.task_id != plan.task_id || binding.plan_version != plan.plan_version {
                bail!("V2 task plan contract set targets another plan");
            }
        }
        let contract_set_sha256 = task_contract_set_sha256(contracts)?;
        Ok(Self {
            schema_version: TASK_STEP_CONTRACT_V2_SCHEMA_VERSION,
            task_id: plan.task_id.clone(),
            plan_version: plan.plan_version,
            contract_count: contracts.len(),
            contract_set_sha256,
        })
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != TASK_STEP_CONTRACT_V2_SCHEMA_VERSION
            || self.plan_version == 0
            || self.contract_count == 0
        {
            bail!("invalid V2 task plan contract-set commit marker");
        }
        let Some(digest) = self.contract_set_sha256.strip_prefix("sha256:") else {
            bail!("task plan contract-set hash must use sha256 prefix");
        };
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("task plan contract-set hash is invalid");
        }
        Ok(())
    }
}

pub(crate) fn task_contract_set_sha256(
    contracts: &[TaskStepContractBoundEntryV2],
) -> Result<String> {
    let mut canonical = contracts.to_vec();
    canonical.sort_by(|left, right| left.step_id.cmp(&right.step_id));
    Ok(format!(
        "sha256:{}",
        crate::sha256_hex(&serde_json::to_vec(&canonical)?)
    ))
}

impl TaskStepContractBoundEntryV2 {
    /// Validates the sidecar independently of replay order.
    pub fn validate(&self) -> Result<()> {
        if self.plan_version == 0 {
            bail!("task step contract plan version must be at least one");
        }
        self.contract.validate()
    }
}

/// Resolves one step contract against the exact visible tool generations selected for a run.
///
/// # Errors
///
/// Returns an error listing every missing semantic capability. Callers must run this immediately
/// before participant launch so a same-name registry replacement cannot bypass admission.
pub fn validate_task_step_capability_admission(
    contract: &TaskStepContractV2,
    tool_contracts: &[crate::ToolRuntimeContract],
) -> Result<()> {
    contract.validate()?;
    let available = tool_contracts
        .iter()
        .flat_map(|tool| tool.capabilities.iter().copied())
        .collect::<BTreeSet<_>>();
    let missing = contract
        .required_capabilities
        .iter()
        .copied()
        .filter(|capability| !available.contains(&capability.tool_capability()))
        .map(TaskCapabilityV2::as_str)
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "task step is missing required capabilities: {}",
            missing.join(", ")
        );
    }
    Ok(())
}

fn validate_contract_item_count(label: &str, count: usize) -> Result<()> {
    if count > TASK_STEP_CONTRACT_MAX_ITEMS {
        bail!("task step contract has too many {label}");
    }
    Ok(())
}

fn validate_contract_text(label: &str, value: &str, max_chars: usize) -> Result<()> {
    if value.trim().is_empty()
        || value.chars().count() > max_chars
        || crate::safe_persistence_text(value) != value
    {
        bail!("task step contract {label} is not safely bounded");
    }
    Ok(())
}

fn validate_contract_workspace_path(value: &str) -> Result<()> {
    validate_contract_text("target path", value, TASK_STEP_CONTRACT_MAX_PATH_CHARS)?;
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("task step contract target path must be workspace-relative");
    }
    Ok(())
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
        if step.intent_refs.iter().collect::<BTreeSet<_>>().len() != step.intent_refs.len() {
            bail!("task step {} repeats an intent ref", step.step_id.as_str());
        }
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
    if role == AgentRole::SubagentWrite
        && !matches!(
            isolation,
            TaskIsolationMode::ChangesetOnly | TaskIsolationMode::Worktree
        )
    {
        bail!(
            "subagent_write task step {} requires changeset_only or worktree isolation; use executor for sequential_workspace_write edits",
            step_id.as_str()
        );
    }
    if role != AgentRole::SubagentWrite
        && matches!(
            isolation,
            TaskIsolationMode::ChangesetOnly | TaskIsolationMode::Worktree
        )
    {
        bail!(
            "{} task step {} requires subagent_write role",
            isolation.as_str(),
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

/// Append-only task run lifecycle entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskRunEntry {
    pub task_id: TaskId,
    pub parent_session_ref: SessionRef,
    pub objective: String,
    /// User-facing semantic title (e.g. the approved plan summary or the routed objective);
    /// absent for legacy or internal-only runs, which fall back to the task id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
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

/// Recovery-critical exact Task focus selected by one explicit continuation invocation.
///
/// Ordinary Task activity never changes conversation focus. This receipt binds the explicit
/// continuation to the root cancellation scope plus the exact pre-dispatch Task/plan facts, so a
/// replay can restore focus without treating a late `TaskRun(Running)` as user intent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskRunTargetSelectedEntry {
    pub selection_id: String,
    pub task_id: TaskId,
    pub run_scope_id: String,
    pub task_status: TaskRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_status: Option<TaskPlanStatus>,
}

impl TaskRunTargetSelectedEntry {
    #[must_use]
    pub fn new(
        task_id: TaskId,
        run_scope_id: impl Into<String>,
        task_status: TaskRunStatus,
        plan_version: Option<u32>,
        plan_status: Option<TaskPlanStatus>,
    ) -> Self {
        let run_scope_id = run_scope_id.into();
        let selection_id = task_run_target_selection_id(&task_id, &run_scope_id);
        Self {
            selection_id,
            task_id,
            run_scope_id,
            task_status,
            plan_version,
            plan_status,
        }
    }

    /// Validates the stable invocation identity and exact pre-dispatch Task facts.
    pub fn validate_shape(&self) -> Result<()> {
        TaskId::new(self.task_id.as_str())?;
        if self.run_scope_id.is_empty()
            || self.run_scope_id.len() > 256
            || self.run_scope_id.chars().any(char::is_whitespace)
        {
            bail!("task run target selection has an invalid run scope");
        }
        if self.selection_id != task_run_target_selection_id(&self.task_id, &self.run_scope_id) {
            bail!("task run target selection identity does not match its binding");
        }
        if !matches!(
            self.task_status,
            TaskRunStatus::Started
                | TaskRunStatus::Running
                | TaskRunStatus::Paused
                | TaskRunStatus::Failed
                | TaskRunStatus::Interrupted
        ) {
            bail!("task run target selection is not resumable");
        }
        if self.plan_version.is_some_and(|version| version == 0) {
            bail!("task run target selection plan version must be non-zero");
        }
        if self.plan_version.is_none() != self.plan_status.is_none() {
            bail!("task run target selection plan version and status must be present together");
        }
        Ok(())
    }
}

#[must_use]
fn task_run_target_selection_id(task_id: &TaskId, run_scope_id: &str) -> String {
    crate::stable_event_uuid(
        TASK_RUN_TARGET_SELECTION_DOMAIN,
        &format!("{}\n{run_scope_id}", task_id.as_str()),
    )
}

/// Exact execution authority rendered with a Task pause action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TaskExecutionBindingV1 {
    /// One first-class direct-execution admission.
    Direct { admission_id: String },
}

impl TaskExecutionBindingV1 {
    fn validate(&self) -> bool {
        match self {
            Self::Direct { admission_id } => {
                !admission_id.is_empty()
                    && admission_id.len() <= 256
                    && !admission_id.chars().any(char::is_control)
            }
        }
    }
}

/// Exact user action that pauses one admitted Task execution incarnation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskPauseRequest {
    pub request_id: String,
    pub task_id: TaskId,
    pub execution: TaskExecutionBindingV1,
}

impl TaskPauseRequest {
    /// Creates a pause request bound to first-class direct-execution authority.
    #[must_use]
    pub fn direct(task_id: TaskId, admission_id: impl Into<String>) -> Self {
        let mut request = Self {
            request_id: String::new(),
            task_id,
            execution: TaskExecutionBindingV1::Direct {
                admission_id: admission_id.into(),
            },
        };
        request.request_id = request.expected_request_id();
        request
    }

    #[must_use]
    pub fn expected_request_id(&self) -> String {
        let seed = serde_json::json!({
            "task_id": self.task_id,
            "execution": self.execution,
        })
        .to_string();
        format!("task-pause-{}", crate::sha256_hex(seed.as_bytes()))
    }

    #[must_use]
    pub fn has_exact_identity(&self) -> bool {
        self.execution.validate() && self.request_id == self.expected_request_id()
    }
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

/// Append-only lifecycle record for a task participant transcript.
///
/// A step participant normally owns the deterministic child-session reference derived from its
/// own attempt id. A later step in a runtime-derived execution segment may instead reference the
/// immediately preceding completed step's transcript. The individual attempt identity and all
/// task-step lifecycle records remain distinct.
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
    /// Returns an error when step facts are inconsistent.
    pub fn validate_shape(&self) -> Result<()> {
        if self.ordinal == 0 {
            bail!("task participant attempt ordinal must start at one");
        }
        if self.plan_version.is_none() || self.step_id.is_none() {
            bail!("step participant attempt is missing plan or step identity");
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

/// Returns the child transcript that a step may continue as part of a runtime-derived execution
/// segment.
///
/// This deliberately accepts only exact linear successors with matching role, mode, isolation,
/// and committed capability authority. Isolated children are excluded because their integration
/// boundary is independent from the model transcript.
pub(crate) fn execution_segment_continuation_session_ref(
    task: &TaskRunProjection,
    plan_version: u32,
    step: &TaskStepSpec,
) -> Option<SessionRef> {
    let plan = task.plans.get(&plan_version)?;
    let segment = derive_task_execution_segments(&plan.steps)
        .into_iter()
        .find(|segment| segment.step_ids.contains(&step.step_id))?;
    let position = segment
        .step_ids
        .iter()
        .position(|step_id| step_id == &step.step_id)?;
    let predecessor_id = segment.step_ids.get(position.checked_sub(1)?)?.clone();
    let predecessor = plan
        .steps
        .iter()
        .find(|candidate| candidate.step_id == predecessor_id)?;
    if step.depends_on.as_slice() != std::slice::from_ref(&predecessor_id)
        || predecessor.role != step.role
        || predecessor.effective_mode() != step.effective_mode()
        || predecessor.effective_isolation() != step.effective_isolation()
        || !matches!(
            step.effective_isolation(),
            TaskIsolationMode::SharedReadOnly | TaskIsolationMode::SequentialWorkspaceWrite
        )
        || task
            .steps
            .get(&(plan_version, predecessor_id.clone()))
            .is_none_or(|state| state.status != TaskStepStatus::Completed)
        || !same_segment_invocation_authority(plan, &predecessor_id, &step.step_id)
    {
        return None;
    }
    task.participant_attempts
        .values()
        .filter(|attempt| {
            attempt.purpose == TaskParticipantPurpose::Step
                && attempt.plan_version == Some(plan_version)
                && attempt.step_id.as_ref() == Some(&predecessor_id)
                && attempt.role == step.role
                && attempt.status == TaskParticipantAttemptStatus::Completed
        })
        .max_by_key(|attempt| attempt.ordinal)
        .map(|attempt| attempt.child_session_ref.clone())
}

fn same_segment_invocation_authority(
    plan: &TaskPlanProjection,
    predecessor_id: &TaskStepId,
    successor_id: &TaskStepId,
) -> bool {
    if !plan.contract_set_committed_v2 {
        return plan.step_contracts.is_empty();
    }
    let Some(predecessor) = plan.step_contracts.get(predecessor_id) else {
        return false;
    };
    let Some(successor) = plan.step_contracts.get(successor_id) else {
        return false;
    };
    predecessor.required_capabilities == successor.required_capabilities
}

/// Bounded result committed from a participant-owned transcript into the parent task log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskParticipantResultEntry {
    pub attempt_id: TaskParticipantAttemptId,
    pub task_id: TaskId,
    pub summary: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub summary_truncated: bool,
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

/// Exact, secret-free identity binding for one subagent approval route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskApprovalRouteBinding {
    pub batch_id: String,
    pub source_thread_id: AgentThreadId,
    pub attempt_id: TaskParticipantAttemptId,
    pub permission_signature: String,
    pub policy_fingerprint: String,
    pub aggregation_signature: String,
    pub source_workspace_id: String,
    pub isolation: TaskIsolationMode,
    pub requested_at_ms: u64,
    pub expires_at_ms: u64,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<TaskApprovalRouteBinding>,
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
    /// Task owned by the latest durable conversation/run focus, if that focus is a Task.
    pub current_task_id: Option<TaskId>,
    pub task_replay_order: Vec<TaskId>,
    /// Exact continuation-focus receipts that conflicted with the task facts visible at replay.
    pub focus_conflicts: usize,
    focus_explicitly_selected: bool,
    task_run_scopes: BTreeMap<TaskId, String>,
    /// Exact Direct Task owners for background agent invocations admitted from this session.
    direct_task_background_owners: BTreeMap<AgentThreadId, TaskId>,
    /// Latest durable lifecycle status for each agent thread observed during replay.
    agent_thread_statuses: BTreeMap<AgentThreadId, crate::AgentThreadStatus>,
    /// Child result records may follow the thread's completed status by a short commit window.
    /// A Direct Task must not treat that status as collected until its result is durable.
    agent_thread_results: BTreeSet<AgentThreadId>,
}

impl TaskStateProjection {
    /// Replays session entries into the latest task projection.
    pub fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            projection.apply_session_entry(entry);
        }
        projection
    }

    pub(crate) fn apply_session_entry(&mut self, entry: &SessionLogEntry) {
        match entry {
            SessionLogEntry::User(_) => self.clear_current_task(),
            SessionLogEntry::Control(control) => self.apply_control_entry(control),
            SessionLogEntry::Assistant(_)
            | SessionLogEntry::RuntimeContextSnapshotV2(_)
            | SessionLogEntry::ToolResultV3(_) => {}
        }
    }

    pub fn latest_task(&self) -> Option<&TaskRunProjection> {
        self.latest_task_id
            .as_ref()
            .and_then(|task_id| self.tasks.get(task_id))
    }

    /// Returns the Task selected by the latest durable run focus.
    pub fn current_task(&self) -> Option<&TaskRunProjection> {
        self.current_task_id
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
            ControlEntry::ConversationInputPromoted(_) => self.clear_current_task(),
            ControlEntry::PlanDraftCreated(_) => self.clear_current_task(),
            ControlEntry::ConversationRouteDecisionRecorded(entry)
                if matches!(
                    entry.route,
                    crate::ConversationRoute::Chat | crate::ConversationRoute::PlanReview
                ) =>
            {
                self.clear_current_task();
            }
            ControlEntry::PlanReviewAttempt(entry)
                if entry.status == crate::PlanReviewAttemptStatus::Started =>
            {
                self.clear_current_task();
            }
            ControlEntry::TaskHandoffResolved(entry)
                if entry.decision == crate::TaskHandoffDecision::Accepted =>
            {
                if let Some(task_id) = entry.task_id.as_ref() {
                    self.select_current_task(task_id);
                }
            }
            ControlEntry::TaskCreatedFromPlan(entry) => {
                self.select_current_task(&entry.task_id);
            }
            ControlEntry::TaskContinuationSelected(entry) => {
                self.apply_continuation_focus(entry);
            }
            ControlEntry::TaskGuidancePromoted(entry) => {
                self.apply_guidance_focus(entry);
            }
            ControlEntry::TaskRunCancellationScopeBound(entry) => {
                self.task_run_scopes
                    .insert(entry.task_id.clone(), entry.run_scope_id.clone());
            }
            ControlEntry::TaskRunTargetSelected(entry) => {
                self.apply_run_target_focus(entry);
            }
            ControlEntry::TaskRun(entry) => self.apply_run(entry),
            ControlEntry::TaskDirectExecutionAdmittedV1(entry) => {
                self.apply_direct_execution_admission(entry)
            }
            ControlEntry::TaskDirectExecutionAttemptV1(entry) => {
                self.apply_direct_execution_attempt(entry)
            }
            ControlEntry::AgentDelegationAdmitted(entry) => {
                self.apply_direct_task_background_admission(entry)
            }
            ControlEntry::AgentThreadStarted(entry) => {
                self.agent_thread_statuses
                    .insert(entry.thread_id.clone(), crate::AgentThreadStatus::Started);
            }
            ControlEntry::AgentThreadStatusChanged(entry) => {
                self.agent_thread_statuses
                    .insert(entry.thread_id.clone(), entry.status);
            }
            ControlEntry::AgentRunInterrupted(entry) => {
                let status = self
                    .agent_thread_statuses
                    .entry(entry.thread_id.clone())
                    .or_insert(crate::AgentThreadStatus::Interrupted);
                if !status.is_terminal() {
                    *status = crate::AgentThreadStatus::Interrupted;
                }
            }
            ControlEntry::AgentThreadResultRecorded(entry) => {
                self.agent_thread_results
                    .insert(entry.result.thread_id.clone());
                self.agent_thread_statuses.insert(
                    entry.result.thread_id.clone(),
                    match entry.result.status {
                        crate::AgentThreadTerminalStatus::Completed => {
                            crate::AgentThreadStatus::Completed
                        }
                        crate::AgentThreadTerminalStatus::Blocked => {
                            crate::AgentThreadStatus::Blocked
                        }
                        crate::AgentThreadTerminalStatus::Failed => {
                            crate::AgentThreadStatus::Failed
                        }
                        crate::AgentThreadTerminalStatus::Cancelled => {
                            crate::AgentThreadStatus::Cancelled
                        }
                        crate::AgentThreadTerminalStatus::Interrupted => {
                            crate::AgentThreadStatus::Interrupted
                        }
                        crate::AgentThreadTerminalStatus::Unknown => {
                            crate::AgentThreadStatus::Unknown
                        }
                    },
                );
            }
            ControlEntry::AgentThreadClosed(entry) => {
                self.agent_thread_statuses
                    .insert(entry.thread_id.clone(), crate::AgentThreadStatus::Closed);
            }
            ControlEntry::TaskChecklistUpdatedV1(entry) => self.apply_checklist(entry),
            ControlEntry::TaskPlan(entry) => self.apply_plan(entry),
            ControlEntry::TaskStepContractBoundV2(entry) => self.apply_step_contract(entry),
            ControlEntry::TaskPlanContractSetCommittedV2(entry) => {
                self.apply_contract_set_commit(entry)
            }
            ControlEntry::TaskStep(entry) => self.apply_step(entry),
            ControlEntry::TaskParticipantAttempt(entry) => self.apply_participant_attempt(entry),
            ControlEntry::TaskParticipantResult(entry) => self.apply_participant_result(entry),
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

    fn apply_continuation_focus(&mut self, entry: &crate::TaskContinuationSelectedEntry) {
        if entry.validate_shape().is_err() {
            self.focus_conflicts = self.focus_conflicts.saturating_add(1);
            return;
        }
        let Some(task) = self.tasks.get(&entry.task_id) else {
            self.focus_conflicts = self.focus_conflicts.saturating_add(1);
            return;
        };
        if task.status != entry.task_status || task.latest_plan_version.is_some() {
            self.focus_conflicts = self.focus_conflicts.saturating_add(1);
            return;
        }
        self.select_current_task(&entry.task_id);
    }

    fn apply_guidance_focus(&mut self, entry: &crate::TaskGuidancePromotedEntry) {
        if entry.validate_shape().is_err() {
            self.focus_conflicts = self.focus_conflicts.saturating_add(1);
            return;
        }
        let Some(task) = self.tasks.get(&entry.task_id) else {
            self.focus_conflicts = self.focus_conflicts.saturating_add(1);
            return;
        };
        if matches!(
            task.status,
            TaskRunStatus::Completed | TaskRunStatus::Cancelled
        ) || task.latest_plan_version.is_some()
            || task
                .direct_execution_admission
                .as_ref()
                .is_none_or(|admission| {
                    admission.validate().is_err() || !admission.matches_objective(&task.objective)
                })
        {
            self.focus_conflicts = self.focus_conflicts.saturating_add(1);
            return;
        }
        self.select_current_task(&entry.task_id);
    }

    fn apply_run_target_focus(&mut self, entry: &TaskRunTargetSelectedEntry) {
        if entry.validate_shape().is_err()
            || self.task_run_scopes.get(&entry.task_id) != Some(&entry.run_scope_id)
        {
            self.focus_conflicts = self.focus_conflicts.saturating_add(1);
            return;
        }
        let Some(task) = self.tasks.get(&entry.task_id) else {
            self.focus_conflicts = self.focus_conflicts.saturating_add(1);
            return;
        };
        let plan_status = entry
            .plan_version
            .and_then(|version| task.plans.get(&version).map(|plan| plan.status));
        if task.status != entry.task_status
            || task.latest_plan_version != entry.plan_version
            || plan_status != entry.plan_status
        {
            self.focus_conflicts = self.focus_conflicts.saturating_add(1);
            return;
        }
        self.select_current_task(&entry.task_id);
    }

    fn apply_run(&mut self, entry: &TaskRunEntry) {
        let task_is_new = !self.tasks.contains_key(&entry.task_id);
        if task_is_new && entry.status == TaskRunStatus::Completed {
            // A current-schema completion is a verdict over an existing direct/DAG authority.
            // Never let a standalone terminal record manufacture both the Task and its success.
            return;
        }
        self.record_task_replay(
            &entry.task_id,
            task_is_new && entry.status == TaskRunStatus::Started,
        );
        let completion_allowed = entry.status != TaskRunStatus::Completed
            || self
                .evaluate_root_terminal(&entry.task_id, TaskRunStatus::Completed, None)
                .is_some_and(|evaluation| evaluation.allows_completed());
        let task = self
            .tasks
            .entry(entry.task_id.clone())
            .or_insert_with(|| TaskRunProjection::from_run(entry));
        if task.status.is_final() && entry.status != task.status {
            task.duplicate_terminal_entries += usize::from(entry.status.is_terminal());
            return;
        }
        if !completion_allowed {
            // Keep the malformed durable claim observable in the append-only log, but never let
            // it manufacture a projected Completed state. The runtime root finalizer appends the
            // resumable terminal selected by the same evaluator.
            task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            return;
        }
        task.objective = entry.objective.clone();
        task.parent_session_ref = entry.parent_session_ref.clone();
        if task.title.is_none() {
            task.title = entry.title.clone();
        }
        task.status = entry.status;
        task.reason = entry.reason.clone();
        if entry.status.is_terminal() {
            task.active_steps.clear();
            task.current_step = None;
        }
    }

    fn apply_plan(&mut self, entry: &TaskPlanEntry) {
        self.record_task_replay(&entry.task_id, false);
        let task = self.ensure_task(&entry.task_id);
        if task.direct_execution_admission.is_some() {
            task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            return;
        }
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
        let graph_validation_error = validate_task_plan_graph_steps(&entry.steps)
            .err()
            .map(|error| error.to_string());
        task.plans.insert(
            entry.plan_version,
            TaskPlanProjection {
                plan_version: entry.plan_version,
                status: entry.status,
                steps: entry.steps.clone(),
                step_contracts: BTreeMap::new(),
                contract_set_committed_v2: false,
                graph_validation_error,
                reason: entry.reason.clone(),
            },
        );
    }

    fn apply_direct_execution_admission(&mut self, entry: &crate::TaskDirectExecutionAdmittedV1) {
        self.record_task_replay(&entry.task_id, false);
        let task = self.ensure_task(&entry.task_id);
        if entry.validate().is_err()
            || !entry.matches_objective(&task.objective)
            || task.latest_plan_version.is_some()
        {
            task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            return;
        }
        match task.direct_execution_admission.as_ref() {
            Some(existing) if existing != entry => {
                task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            }
            Some(_) => {}
            None => task.direct_execution_admission = Some(entry.clone()),
        }
    }

    fn apply_direct_execution_attempt(&mut self, entry: &crate::TaskDirectExecutionAttemptV1) {
        self.record_task_replay(&entry.task_id, false);
        let task = self.ensure_task(&entry.task_id);
        if entry.validate().is_err()
            || task
                .direct_execution_admission
                .as_ref()
                .is_none_or(|admission| admission.admission_id != entry.admission_id)
        {
            task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            return;
        }
        match task.direct_execution_attempts.get(&entry.attempt_id) {
            Some(existing) if existing.status.is_terminal() && existing != entry => {
                task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            }
            _ => {
                task.direct_execution_attempts
                    .insert(entry.attempt_id.clone(), entry.clone());
            }
        }
    }

    fn apply_checklist(&mut self, entry: &crate::TaskChecklistUpdatedV1) {
        self.record_task_replay(&entry.task_id, false);
        let task = self.ensure_task(&entry.task_id);
        if entry.validate().is_err() {
            task.checklist_conflicts = task.checklist_conflicts.saturating_add(1);
            return;
        }
        match task.checklist.as_ref() {
            Some(existing) if entry.revision < existing.revision => {}
            Some(existing) if entry.revision == existing.revision && entry != existing => {
                task.checklist_conflicts = task.checklist_conflicts.saturating_add(1);
            }
            _ => task.checklist = Some(entry.clone()),
        }
    }

    fn apply_step_contract(&mut self, entry: &TaskStepContractBoundEntryV2) {
        self.record_task_replay(&entry.task_id, false);
        if entry.validate().is_err() {
            let task = self.ensure_task(&entry.task_id);
            task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            return;
        }
        let task = self.ensure_task(&entry.task_id);
        let Some(plan) = task.plans.get_mut(&entry.plan_version) else {
            task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            return;
        };
        if !plan.steps.iter().any(|step| step.step_id == entry.step_id) {
            task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            return;
        }
        match plan.step_contracts.get(&entry.step_id) {
            Some(existing) if existing == &entry.contract => {}
            Some(_) => {
                task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            }
            None => {
                plan.step_contracts
                    .insert(entry.step_id.clone(), entry.contract.clone());
            }
        }
    }

    fn apply_contract_set_commit(&mut self, entry: &TaskPlanContractSetCommittedV2) {
        self.record_task_replay(&entry.task_id, false);
        if entry.validate().is_err() {
            let task = self.ensure_task(&entry.task_id);
            task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            return;
        }
        let task = self.ensure_task(&entry.task_id);
        let Some(plan) = task.plans.get_mut(&entry.plan_version) else {
            task.participant_conflicts = task.participant_conflicts.saturating_add(1);
            return;
        };
        let bindings = plan
            .step_contracts
            .iter()
            .map(|(step_id, contract)| TaskStepContractBoundEntryV2 {
                task_id: entry.task_id.clone(),
                plan_version: entry.plan_version,
                step_id: step_id.clone(),
                contract: contract.clone(),
            })
            .collect::<Vec<_>>();
        let plan_entry = TaskPlanEntry {
            task_id: entry.task_id.clone(),
            plan_version: entry.plan_version,
            status: plan.status,
            steps: plan.steps.clone(),
            reason: plan.reason.clone(),
        };
        match TaskPlanContractSetCommittedV2::new(&plan_entry, &bindings) {
            Ok(expected) if expected == *entry => plan.contract_set_committed_v2 = true,
            _ => task.participant_conflicts = task.participant_conflicts.saturating_add(1),
        }
    }

    /// Returns the exact Direct Task that authorized one background agent, when one exists.
    #[must_use]
    pub fn direct_task_for_background_agent(&self, thread_id: &AgentThreadId) -> Option<&TaskId> {
        self.direct_task_background_owners.get(thread_id)
    }

    /// Returns every background agent admitted under this Direct Task's exact grant.
    #[must_use]
    pub fn direct_task_background_agents(&self, task_id: &TaskId) -> Vec<AgentThreadId> {
        self.direct_task_background_owners
            .iter()
            .filter_map(|(thread_id, owner_task_id)| {
                (owner_task_id == task_id).then_some(thread_id.clone())
            })
            .collect()
    }

    /// Returns the latest durable status observed for an agent thread in this Task projection.
    #[must_use]
    pub fn agent_thread_status(
        &self,
        thread_id: &AgentThreadId,
    ) -> Option<crate::AgentThreadStatus> {
        self.agent_thread_statuses.get(thread_id).copied()
    }

    /// Returns nonterminal background agents admitted under this Direct Task's exact grant.
    #[must_use]
    pub fn unfinished_direct_task_background_agents(&self, task_id: &TaskId) -> Vec<AgentThreadId> {
        self.direct_task_background_agents(task_id)
            .into_iter()
            .filter(|thread_id| {
                let status = self
                    .agent_thread_statuses
                    .get(thread_id)
                    .copied()
                    .unwrap_or(crate::AgentThreadStatus::Started);
                if status == crate::AgentThreadStatus::Completed {
                    !self.agent_thread_results.contains(thread_id)
                } else {
                    !status.is_terminal()
                }
            })
            .collect()
    }

    /// Evaluates a root terminal against the current durable projection.
    #[must_use]
    pub fn evaluate_root_terminal(
        &self,
        task_id: &TaskId,
        requested_status: TaskRunStatus,
        candidate: Option<&TaskRootTerminalCandidateV1>,
    ) -> Option<TaskRootTerminalEvaluationV1> {
        self.tasks.get(task_id).map(|task| {
            let mut evaluation = task.evaluate_root_terminal(requested_status, candidate);
            if requested_status == TaskRunStatus::Completed
                && task.direct_execution_admission.is_some()
            {
                evaluation.unfinished_direct_task_background_agents =
                    self.unfinished_direct_task_background_agents(task_id);
                if !evaluation
                    .unfinished_direct_task_background_agents
                    .is_empty()
                {
                    evaluation
                        .completion_blockers
                        .push(TaskRootCompletionBlockerV1::UnfinishedBackgroundAgent);
                    evaluation
                        .completion_blockers
                        .sort_by_key(|blocker| blocker.reason_code());
                    evaluation.completion_blockers.dedup();
                    evaluation.effective_status = TaskRunStatus::Paused;
                }
            }
            evaluation
        })
    }

    fn apply_direct_task_background_admission(
        &mut self,
        entry: &crate::AgentDelegationAdmissionEntry,
    ) {
        if entry.invocation_mode != crate::AgentInvocationMode::Background
            || entry.invocation_source != crate::AgentInvocationSource::Task
        {
            return;
        }
        let (
            crate::DelegationAuthorityRecord::DirectTask {
                task_id: authority_task_id,
            },
            Some(grant),
        ) = (&entry.authority, entry.invocation_grant.as_ref())
        else {
            return;
        };
        let (
            crate::AgentInvocationGrantSource::DirectTask {
                task_id: source_task_id,
            },
            crate::DelegationAuthorityRecord::DirectTask {
                task_id: grant_task_id,
            },
        ) = (&grant.source, &grant.authority)
        else {
            return;
        };
        if authority_task_id != source_task_id
            || authority_task_id != grant_task_id
            || entry.profile_id != grant.profile_id
            || entry.tool_contract_fingerprint != grant.tool_contract_fingerprint
            || self
                .tasks
                .get(authority_task_id)
                .is_none_or(|task| task.direct_execution_admission.is_none())
        {
            return;
        }
        self.direct_task_background_owners
            .insert(entry.thread_id.clone(), authority_task_id.clone());
        self.agent_thread_statuses
            .entry(entry.thread_id.clone())
            .or_insert(crate::AgentThreadStatus::Started);
    }

    /// Projects current Task lifecycle status for product surfaces.
    pub fn execution_phase(&self, task_id: &TaskId) -> Option<TaskExecutionPhaseV1> {
        let task = self.tasks.get(task_id)?;
        Some(match task.status {
            TaskRunStatus::Completed => TaskExecutionPhaseV1::Completed,
            TaskRunStatus::Failed => TaskExecutionPhaseV1::Failed,
            TaskRunStatus::Cancelled => TaskExecutionPhaseV1::Cancelled,
            TaskRunStatus::Interrupted => TaskExecutionPhaseV1::Interrupted,
            TaskRunStatus::Paused => TaskExecutionPhaseV1::Paused,
            TaskRunStatus::Running => TaskExecutionPhaseV1::Running,
            TaskRunStatus::Started => TaskExecutionPhaseV1::Ready,
        })
    }

    fn apply_step(&mut self, entry: &TaskStepEntry) {
        self.record_task_replay(&entry.task_id, false);
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
        self.record_task_replay(&entry.task_id, false);
        let task = self.ensure_task(&entry.task_id);
        let direct_child_session_ref =
            task_participant_session_ref(&entry.task_id, &entry.attempt_id).ok();
        let valid_segment_continuation = entry
            .plan_version
            .and_then(|plan_version| {
                entry.step_id.as_ref().and_then(|step_id| {
                    task.plans
                        .get(&plan_version)
                        .and_then(|plan| plan.steps.iter().find(|step| step.step_id == *step_id))
                        .and_then(|step| {
                            execution_segment_continuation_session_ref(task, plan_version, step)
                        })
                })
            })
            .is_some_and(|session_ref| session_ref == entry.child_session_ref);
        if entry.validate_shape().is_err()
            || (direct_child_session_ref.as_ref() != Some(&entry.child_session_ref)
                && !valid_segment_continuation)
        {
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

    fn apply_participant_result(&mut self, entry: &TaskParticipantResultEntry) {
        self.record_task_replay(&entry.task_id, false);
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

    fn apply_child_session(&mut self, entry: &TaskChildSessionEntry) {
        self.record_task_replay(&entry.task_id, false);
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
        self.record_task_replay(&entry.task_id, false);
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
        self.record_task_replay(&entry.task_id, false);
        let task = self.ensure_task(&entry.task_id);
        let child_matches = task.child_sessions.values().any(|child| {
            child.plan_version == entry.plan_version
                && child.step_id == entry.step_id
                && child.child_session_ref == entry.child_session_ref
        });
        if !child_matches {
            task.route_unverified = true;
        }
        if entry.binding.as_ref().is_none_or(|binding| {
            binding.batch_id.trim().is_empty()
                || binding.source_thread_id.as_str().trim().is_empty()
                || binding.attempt_id.as_str().trim().is_empty()
                || !binding.permission_signature.starts_with("sha256:")
                || !binding.policy_fingerprint.starts_with("sha256:")
                || !binding.aggregation_signature.starts_with("sha256:")
                || !binding.source_workspace_id.starts_with("workspace:")
                || binding.expires_at_ms <= binding.requested_at_ms
        }) {
            task.route_unverified = true;
        }
        task.approval_routes
            .insert(entry.route_id.clone(), entry.clone());
    }

    fn apply_elicitation_route(&mut self, entry: &TaskSubagentElicitationRouteEntry) {
        self.record_task_replay(&entry.task_id, false);
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

    fn clear_current_task(&mut self) {
        self.current_task_id = None;
        self.focus_explicitly_selected = true;
    }

    fn select_current_task(&mut self, task_id: &TaskId) {
        self.current_task_id = Some(task_id.clone());
        self.focus_explicitly_selected = true;
    }

    fn record_task_replay(&mut self, task_id: &TaskId, admit_new_task: bool) {
        self.latest_task_id = Some(task_id.clone());
        if !self.focus_explicitly_selected
            || admit_new_task
            || self.current_task_id.as_ref() == Some(task_id)
        {
            self.current_task_id = Some(task_id.clone());
        }
        self.task_replay_order.push(task_id.clone());
    }
}

/// Projection for one task run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRunProjection {
    pub task_id: TaskId,
    pub parent_session_ref: SessionRef,
    pub objective: String,
    /// User-facing semantic title from the durable task run entry.
    pub title: Option<String>,
    pub status: TaskRunStatus,
    pub reason: Option<String>,
    /// First-class authority for running the complete objective without a TaskPlan.
    pub direct_execution_admission: Option<crate::TaskDirectExecutionAdmittedV1>,
    /// Durable direct-execution attempts keyed by their stable attempt id.
    pub direct_execution_attempts: BTreeMap<String, crate::TaskDirectExecutionAttemptV1>,
    /// Latest display-only checklist. It has no execution or completion authority.
    pub checklist: Option<crate::TaskChecklistUpdatedV1>,
    pub checklist_conflicts: usize,
    pub latest_plan_version: Option<u32>,
    pub plans: BTreeMap<u32, TaskPlanProjection>,
    pub steps: BTreeMap<(u32, TaskStepId), TaskStepProjection>,
    /// All task steps whose latest append-only status is `Running`.
    pub active_steps: BTreeSet<(u32, TaskStepId)>,
    /// Compatibility view populated only when exactly one task step is active.
    pub current_step: Option<(u32, TaskStepId)>,
    pub participant_attempts: BTreeMap<TaskParticipantAttemptId, TaskParticipantAttemptEntry>,
    pub participant_results: BTreeMap<TaskParticipantAttemptId, TaskParticipantResultEntry>,
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
            title: entry.title.clone(),
            status: entry.status,
            reason: entry.reason.clone(),
            direct_execution_admission: None,
            direct_execution_attempts: BTreeMap::new(),
            checklist: None,
            checklist_conflicts: 0,
            latest_plan_version: None,
            plans: BTreeMap::new(),
            steps: BTreeMap::new(),
            active_steps: BTreeSet::new(),
            current_step: None,
            participant_attempts: BTreeMap::new(),
            participant_results: BTreeMap::new(),
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
            title: None,
            status: TaskRunStatus::Started,
            reason: None,
            direct_execution_admission: None,
            direct_execution_attempts: BTreeMap::new(),
            checklist: None,
            checklist_conflicts: 0,
            latest_plan_version: None,
            plans: BTreeMap::new(),
            steps: BTreeMap::new(),
            active_steps: BTreeSet::new(),
            current_step: None,
            participant_attempts: BTreeMap::new(),
            participant_results: BTreeMap::new(),
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

    /// Returns the next attempt ordinal for one participant identity.
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

    /// Evaluates a proposed root terminal from durable Task facts only.
    ///
    /// A direct Task needs its latest direct attempt to be completed. A DAG Task needs every
    /// current-plan step completed, no active participant, and no unresolved blocker. Older
    /// terminal attempts remain audit history and do not poison a later successful retry.
    #[must_use]
    pub fn evaluate_root_terminal(
        &self,
        requested_status: TaskRunStatus,
        candidate: Option<&TaskRootTerminalCandidateV1>,
    ) -> TaskRootTerminalEvaluationV1 {
        let mut blocked_dependency_steps = BTreeSet::new();
        let mut cancelled_dependency_steps = BTreeSet::new();
        let mut cancellation_closure = BTreeSet::new();
        let mut unfinished_steps = BTreeSet::new();
        let mut unfinished_participants = BTreeSet::new();
        let mut unfinished_direct_attempts = BTreeSet::new();
        let mut completion_blockers = Vec::new();

        let has_direct_authority = self.direct_execution_admission.is_some();
        let selected_plan = self
            .latest_plan_version
            .and_then(|version| self.plans.get(&version))
            .filter(|plan| plan.status == TaskPlanStatus::Accepted);
        let has_dag_authority = selected_plan.is_some();

        if has_direct_authority {
            let latest_attempt = self
                .direct_execution_attempts
                .values()
                .max_by_key(|attempt| attempt.ordinal);
            let latest_status = latest_attempt
                .map(|attempt| direct_attempt_status_with_candidate(attempt, candidate));
            if latest_status != Some(TaskExecutionAttemptStatus::Completed) {
                if let Some(attempt) = latest_attempt {
                    unfinished_direct_attempts.insert(attempt.attempt_id.clone());
                } else {
                    unfinished_direct_attempts.insert("direct_execution_not_started".to_owned());
                }
            }
            for attempt in self.direct_execution_attempts.values() {
                if direct_attempt_status_with_candidate(attempt, candidate)
                    == TaskExecutionAttemptStatus::Started
                {
                    unfinished_direct_attempts.insert(attempt.attempt_id.clone());
                }
            }
            if !unfinished_direct_attempts.is_empty() {
                completion_blockers.push(TaskRootCompletionBlockerV1::UnfinishedDirectExecution);
            }
        }

        if let Some(plan) = selected_plan {
            let mut blocking_roots = BTreeSet::new();
            let mut cancelled_roots = BTreeSet::new();
            for step in &plan.steps {
                let status = self
                    .steps
                    .get(&(plan.plan_version, step.step_id.clone()))
                    .map(|projection| projection.status);
                match status {
                    Some(TaskStepStatus::Completed) => {}
                    Some(TaskStepStatus::Failed) => {
                        blocking_roots.insert(step.step_id.clone());
                        completion_blockers.push(TaskRootCompletionBlockerV1::FailedDependency);
                    }
                    Some(TaskStepStatus::Blocked | TaskStepStatus::Interrupted) => {
                        blocking_roots.insert(step.step_id.clone());
                        completion_blockers.push(TaskRootCompletionBlockerV1::BlockedDependency);
                    }
                    Some(TaskStepStatus::Cancelled) => {
                        cancelled_roots.insert(step.step_id.clone());
                        completion_blockers.push(TaskRootCompletionBlockerV1::CancelledDependency);
                    }
                    Some(
                        TaskStepStatus::Pending
                        | TaskStepStatus::Running
                        | TaskStepStatus::Superseded,
                    )
                    | None => {
                        unfinished_steps.insert(step.step_id.clone());
                    }
                }
            }
            blocked_dependency_steps = task_dependency_descendants(
                &plan.steps,
                &blocking_roots,
                &self.steps,
                plan.plan_version,
            );
            cancelled_dependency_steps = task_dependency_descendants(
                &plan.steps,
                &cancelled_roots,
                &self.steps,
                plan.plan_version,
            );
            if !blocked_dependency_steps.is_empty() {
                completion_blockers.push(TaskRootCompletionBlockerV1::BlockedDependency);
            }
            if !unfinished_steps.is_empty() {
                completion_blockers.push(TaskRootCompletionBlockerV1::UnfinishedStep);
            }
            if requested_status == TaskRunStatus::Cancelled {
                for step in &plan.steps {
                    let status = self
                        .steps
                        .get(&(plan.plan_version, step.step_id.clone()))
                        .map(|projection| projection.status);
                    if !matches!(
                        status,
                        Some(
                            TaskStepStatus::Completed
                                | TaskStepStatus::Superseded
                                | TaskStepStatus::Cancelled
                        )
                    ) {
                        cancellation_closure.insert(step.step_id.clone());
                    }
                }
            }
        }

        for attempt in self.participant_attempts.values() {
            if participant_attempt_status_with_candidate(attempt, candidate)
                == TaskParticipantAttemptStatus::Started
            {
                unfinished_participants.insert(attempt.attempt_id.clone());
            }
        }
        if !unfinished_participants.is_empty() {
            completion_blockers.push(TaskRootCompletionBlockerV1::UnfinishedParticipant);
        }

        completion_blockers.sort_by_key(|blocker| blocker.reason_code());
        completion_blockers.dedup();

        let effective_status = if requested_status == TaskRunStatus::Completed
            && (has_direct_authority || has_dag_authority)
            && !completion_blockers.is_empty()
        {
            // A completion claim never infers that a failed or cancelled dependency was an
            // intentional root terminal. Keep the Task resumable until the exact authority
            // resolves its blocker or explicitly requests cancellation.
            TaskRunStatus::Paused
        } else {
            requested_status
        };

        TaskRootTerminalEvaluationV1 {
            requested_status,
            effective_status,
            blocked_dependency_steps: blocked_dependency_steps.into_iter().collect(),
            cancelled_dependency_steps: cancelled_dependency_steps.into_iter().collect(),
            cancellation_closure: cancellation_closure.into_iter().collect(),
            unfinished_steps: unfinished_steps.into_iter().collect(),
            unfinished_participants: unfinished_participants.into_iter().collect(),
            unfinished_direct_attempts: unfinished_direct_attempts.into_iter().collect(),
            unfinished_direct_task_background_agents: Vec::new(),
            completion_blockers,
        }
    }
}

fn direct_attempt_status_with_candidate(
    attempt: &crate::TaskDirectExecutionAttemptV1,
    candidate: Option<&TaskRootTerminalCandidateV1>,
) -> TaskExecutionAttemptStatus {
    match candidate {
        Some(TaskRootTerminalCandidateV1::DirectExecution { attempt_id, status })
            if attempt.attempt_id == *attempt_id =>
        {
            *status
        }
        _ => attempt.status,
    }
}

fn participant_attempt_status_with_candidate(
    attempt: &TaskParticipantAttemptEntry,
    candidate: Option<&TaskRootTerminalCandidateV1>,
) -> TaskParticipantAttemptStatus {
    match candidate {
        Some(TaskRootTerminalCandidateV1::Participant { attempt_id, status })
            if attempt.attempt_id == *attempt_id =>
        {
            *status
        }
        _ => attempt.status,
    }
}

fn task_dependency_descendants(
    steps: &[TaskStepSpec],
    roots: &BTreeSet<TaskStepId>,
    statuses: &BTreeMap<(u32, TaskStepId), TaskStepProjection>,
    plan_version: u32,
) -> BTreeSet<TaskStepId> {
    let mut closure = roots.clone();
    loop {
        let mut changed = false;
        for step in steps {
            if closure.contains(&step.step_id)
                || !step
                    .depends_on
                    .iter()
                    .any(|dependency| closure.contains(dependency))
                || statuses
                    .get(&(plan_version, step.step_id.clone()))
                    .is_some_and(|projection| {
                        matches!(
                            projection.status,
                            TaskStepStatus::Completed | TaskStepStatus::Superseded
                        )
                    })
            {
                continue;
            }
            changed |= closure.insert(step.step_id.clone());
        }
        if !changed {
            break;
        }
    }
    for root in roots {
        closure.remove(root);
    }
    closure
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
    /// V2 sidecars keyed by step. Empty for legacy V1 plans.
    pub step_contracts: BTreeMap<TaskStepId, TaskStepContractV2>,
    /// True only after the exact complete V2 set commit marker replays successfully.
    pub contract_set_committed_v2: bool,
    pub graph_validation_error: Option<String>,
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

/// Returns stale terminals for task approval routes that lost their live interaction owner.
///
/// Recovery never replays the prior decision or preview. A resumed participant must ask again,
/// producing a new attempt identity and a fresh permission signature.
pub fn stale_task_approval_routes_for_restore(
    entries: &[SessionLogEntry],
) -> Vec<TaskSubagentApprovalRouteEntry> {
    let mut routes = BTreeMap::<TaskRouteId, TaskSubagentApprovalRouteEntry>::new();
    for entry in entries {
        let SessionLogEntry::Control(ControlEntry::TaskSubagentApprovalRoute(route)) = entry else {
            continue;
        };
        routes.insert(route.route_id.clone(), route.clone());
    }
    routes
        .into_values()
        .filter_map(|mut route| {
            if !matches!(
                route.status,
                TaskRouteStatus::Registered | TaskRouteStatus::Requested
            ) {
                return None;
            }
            route.status = TaskRouteStatus::Stale;
            Some(route)
        })
        .collect()
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

/// Maximum characters of a generated user-facing task title.
pub const TASK_SEMANTIC_TITLE_MAX_CHARS: usize = 64;

/// Builds a bounded, persistence-safe user-facing task title from a semantic source (approved
/// plan summary or routed objective). Falls back to a stable neutral label when the source is
/// empty after safe projection.
#[must_use]
pub fn task_semantic_title(source: &str) -> String {
    let safe = crate::safe_persistence_text(source);
    let summary = safe.lines().find_map(|line| {
        line.trim()
            .strip_prefix("Summary:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
    });
    let first_line = safe.lines().map(str::trim).find(|line| !line.is_empty());
    let mut title = summary.or(first_line).unwrap_or_default().to_owned();
    if title.chars().count() > TASK_SEMANTIC_TITLE_MAX_CHARS {
        title = format!(
            "{}…",
            title
                .chars()
                .take(TASK_SEMANTIC_TITLE_MAX_CHARS)
                .collect::<String>()
        );
    }
    if title.is_empty() {
        "task".to_owned()
    } else {
        title
    }
}
