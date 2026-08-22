use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Digest;

use crate::{
    AgentArtifactRef, AgentFinalAnswerRef, AgentThreadId,
    provider::ToolCall,
    session::{ControlEntry, SessionLogEntry},
    tool::{ToolAccess, ToolCategory, ToolPreviewCapability, ToolSpec},
};

pub const TASK_PLAN_UPDATE_TOOL_NAME: &str = "task_plan_update";
pub const TASK_GUIDANCE_APPLY_TOOL_NAME: &str = "task_guidance_apply";
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
/// Maximum automatic provider-pressure retries for one task participant identity.
pub const MAX_TASK_PARTICIPANT_AUTO_RETRIES: usize = 2;
/// Maximum cumulative delay admitted for automatic retries of one participant identity.
pub const MAX_TASK_PARTICIPANT_AUTO_RETRY_WAIT_MS: u64 = 120_000;
/// Repeating the same semantic call batch against the same result frontier twice requests a
/// bounded participant finalization turn instead of allowing another analysis loop.
pub const TASK_STEP_NO_PROGRESS_FINALIZE_THRESHOLD: u32 = 2;

const TASK_PARTICIPANT_ATTEMPT_ID_DOMAIN: &str = "sigil-task-participant-attempt-v1";
const TASK_PARTICIPANT_CHILD_ID_DOMAIN: &str = "sigil-task-participant-child-v1";
const TASK_FINAL_MESSAGE_ID_DOMAIN: &str = "sigil-task-final-message-v1";
const TASK_RUN_TARGET_SELECTION_DOMAIN: &str = "sigil-task-run-target-selection-v1";
const TASK_GUIDANCE_MATERIALIZATION_DOMAIN: &str = "sigil-task-guidance-materialization-v1";

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

/// Durable execution phase of a Task adopted through the single execution spine (RFC-0067).
///
/// `Preparing` is the initial phase of every adopted Task; admission transitions it to `Ready`,
/// `Blocked` or `Paused`. `Blocked` is recoverable and never means the Task is gone.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskExecutionPhaseV1 {
    Preparing,
    Ready,
    Running,
    Blocked,
    Paused,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl TaskExecutionPhaseV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Blocked => "blocked",
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

/// One monotonic admission attempt of an adopted Task (RFC-0067 10.2).
///
/// Every resume or relevant environment change appends a higher ordinal; historical attempts are
/// never overwritten.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskAdmissionAttemptV1 {
    pub task_id: TaskId,
    pub plan_version: u32,
    pub ordinal: u32,
    pub candidate_hash: String,
    pub observed_environment: TaskAdmissionObservationV1,
    pub outcome: TaskAdmissionOutcomeV1,
}

impl TaskAdmissionAttemptV1 {
    /// Validates the durable admission record is bounded and self-consistent.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero ordinal or plan version, an invalid candidate digest,
    /// unbounded blocker text, or a malformed lease/evidence digest.
    pub fn validate(&self) -> Result<()> {
        if self.ordinal == 0 {
            bail!("task admission ordinal must start at one");
        }
        if self.plan_version == 0 {
            bail!("task admission plan version must start at one");
        }
        let digest = self
            .candidate_hash
            .strip_prefix("sha256:")
            .unwrap_or(&self.candidate_hash);
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("task admission candidate hash is not a sha256 digest");
        }
        self.observed_environment.validate()?;
        match &self.outcome {
            TaskAdmissionOutcomeV1::Ready(lease) => {
                if lease.lease_id.is_empty()
                    || lease.lease_id.len() > 128
                    || crate::safe_persistence_text(&lease.lease_id) != lease.lease_id
                    || !lease.lease_id.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':')
                    })
                {
                    bail!("task admission lease id is not a bounded safe identity");
                }
            }
            TaskAdmissionOutcomeV1::Blocked(blocker) => blocker.validate()?,
            TaskAdmissionOutcomeV1::Paused(_) => {}
        }
        let size = serde_json::to_vec(self).context("failed to size task admission attempt")?;
        if size.len() > MAX_TASK_ADMISSION_RECORD_BYTES {
            bail!(
                "task admission attempt exceeds maximum of {} bytes",
                MAX_TASK_ADMISSION_RECORD_BYTES
            );
        }
        Ok(())
    }
}

/// Maximum serialized size of one durable Task admission attempt.
pub const MAX_TASK_ADMISSION_RECORD_BYTES: usize = 64 * 1024;

impl TaskAdmissionObservationV1 {
    /// Validates the environment observation is bounded.
    ///
    /// # Errors
    ///
    /// Returns an error for unbounded snapshot ids or duplicated capabilities.
    pub fn validate(&self) -> Result<()> {
        for (label, value) in [
            (
                "admission base workspace snapshot",
                self.base_workspace_snapshot_id.as_deref(),
            ),
            (
                "admission current workspace snapshot",
                self.current_workspace_snapshot_id.as_deref(),
            ),
        ] {
            if let Some(value) = value {
                validate_admission_snapshot_id(label, value)?;
            }
        }
        if self
            .missing_capabilities
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != self.missing_capabilities.len()
        {
            bail!("task admission repeats a missing capability");
        }
        if self.missing_capabilities.len() > TASK_STEP_CONTRACT_MAX_ITEMS {
            bail!("task admission missing capabilities exceed maximum count");
        }
        Ok(())
    }
}

fn validate_admission_snapshot_id(label: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 128 || crate::safe_persistence_text(value) != value {
        bail!("{label} is not bounded safe text");
    }
    Ok(())
}

/// Environment facts observed by one Task admission attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskAdmissionObservationV1 {
    pub base_workspace_snapshot_id: Option<String>,
    pub current_workspace_snapshot_id: Option<String>,
    pub workspace_state: WorkspaceAdmissionStateV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_capabilities: Vec<TaskCapabilityV2>,
    pub provider_route_available: bool,
    pub credential_available: bool,
    pub permission_profile_ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_space_bytes: Option<u64>,
    pub external_writer_active: bool,
    pub verification_runner_available: bool,
    pub observed_at_ms: u64,
}

/// Workspace relationship observed at admission time.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAdmissionStateV1 {
    /// Current snapshot equals the candidate's base snapshot.
    ExactMatch,
    /// The workspace changed but every mutation is already audited against this Task.
    AuditedSelfMutation,
    /// The workspace changed without an audited cause for this Task.
    ExternalDrift,
    /// No current snapshot could be produced.
    SnapshotUnavailable,
}

/// Typed admission outcome for one attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum TaskAdmissionOutcomeV1 {
    Ready(TaskRuntimeLeaseBindingV1),
    Blocked(TaskBlockerV1),
    Paused(TaskPauseReasonV1),
}

/// Lease binding returned when admission grants a Task runtime start.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskRuntimeLeaseBindingV1 {
    pub lease_id: String,
    pub granted_at_ms: u64,
}

/// Recoverable blocker produced by Task admission (RFC-0067 10.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskBlockerV1 {
    pub reason_code: TaskBlockerReasonCodeV1,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_step: Option<TaskStepId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_capability: Option<TaskCapabilityV2>,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_actions: Vec<TaskBlockerActionV1>,
    pub evidence_digest: String,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at_ms: Option<u64>,
}

impl TaskBlockerV1 {
    pub fn is_resolved(&self) -> bool {
        self.resolved_at_ms.is_some()
    }

    /// Validates the blocker record is bounded and self-consistent.
    ///
    /// # Errors
    ///
    /// Returns an error for unbounded text, an invalid evidence digest, or an impossible
    /// created/resolved ordering.
    pub fn validate(&self) -> Result<()> {
        if self.summary.is_empty()
            || self.summary.len() > TASK_STEP_CONTRACT_MAX_TEXT_CHARS
            || crate::safe_persistence_text(&self.summary) != self.summary
        {
            bail!("task blocker summary is not bounded safe text");
        }
        let digest = self
            .evidence_digest
            .strip_prefix("sha256:")
            .unwrap_or(&self.evidence_digest);
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("task blocker evidence digest is not a sha256 digest");
        }
        if self.available_actions.len() > 8 {
            bail!("task blocker available actions exceed maximum count");
        }
        if self
            .resolved_at_ms
            .is_some_and(|resolved| resolved < self.created_at_ms)
        {
            bail!("task blocker resolved before it was created");
        }
        Ok(())
    }
}

/// Stable blocker reason codes (RFC-0067 10.3).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskBlockerReasonCodeV1 {
    WorkspaceChanged,
    WorkspaceSnapshotUnavailable,
    MissingRequiredCapability,
    ProviderUnavailable,
    CredentialUnavailable,
    PermissionRequired,
    WorkspaceTrustRequired,
    ExternalWriterActive,
    IsolationUnavailable,
    DiskSpaceExhausted,
    ArtifactStorageUnavailable,
    SessionStorageDegraded,
    VerificationRunnerUnavailable,
    RouteRebindRequired,
    ContractRecompileRequired,
}

impl TaskBlockerReasonCodeV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceChanged => "workspace_changed",
            Self::WorkspaceSnapshotUnavailable => "workspace_snapshot_unavailable",
            Self::MissingRequiredCapability => "missing_required_capability",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::CredentialUnavailable => "credential_unavailable",
            Self::PermissionRequired => "permission_required",
            Self::WorkspaceTrustRequired => "workspace_trust_required",
            Self::ExternalWriterActive => "external_writer_active",
            Self::IsolationUnavailable => "isolation_unavailable",
            Self::DiskSpaceExhausted => "disk_space_exhausted",
            Self::ArtifactStorageUnavailable => "artifact_storage_unavailable",
            Self::SessionStorageDegraded => "session_storage_degraded",
            Self::VerificationRunnerUnavailable => "verification_runner_unavailable",
            Self::RouteRebindRequired => "route_rebind_required",
            Self::ContractRecompileRequired => "contract_recompile_required",
        }
    }
}

/// Typed action a user may take on an active blocker.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskBlockerActionV1 {
    RetryAdmission,
    Replan,
    Cancel,
    RebindRoute,
    GrantPermission,
    Resume,
}

impl TaskBlockerActionV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RetryAdmission => "retry_admission",
            Self::Replan => "replan",
            Self::Cancel => "cancel",
            Self::RebindRoute => "rebind_route",
            Self::GrantPermission => "grant_permission",
            Self::Resume => "resume",
        }
    }
}

/// Durable reason a Task is paused (RFC-0067 10.1).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskPauseReasonV1 {
    UserRequested,
    CreatePaused,
    AdmissionHeld,
}

impl TaskPauseReasonV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserRequested => "user_requested",
            Self::CreatePaused => "create_paused",
            Self::AdmissionHeld => "admission_held",
        }
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

/// Continuation boundary attached to one materialized execution segment.
///
/// A segment only joins a direct dependency chain. It never turns parallel work into a shared
/// transcript and never expands the authority granted to an individual step.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationContractV1 {
    ExactLinearSameAuthority,
}

/// Checkpoint cadence required before a segment may continue after interruption.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SegmentCheckpointPolicyV1 {
    EveryProviderTurn,
}

/// RFC-0069 durable execution-segment receipt carried by Task materialization.
///
/// Individual steps retain their own lifecycle and completion proof. This record only proves
/// which exact linear steps may reuse a participant/provider continuity boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ExecutionSegmentV1 {
    pub segment_id: String,
    pub task_id: TaskId,
    pub plan_version: u32,
    pub ordered_step_ids: Vec<TaskStepId>,
    pub role: AgentRole,
    pub authority_fingerprint: String,
    pub isolation: TaskIsolationMode,
    pub continuation_contract: ContinuationContractV1,
    pub checkpoint_policy: SegmentCheckpointPolicyV1,
}

/// Materializes the exact segment receipts for an executable candidate.
///
/// The authority fingerprint covers role, effective mode, isolation and the resolved V2
/// capability set. This makes a candidate produced under a different task contract unable to
/// silently reuse an earlier participant transcript.
#[must_use]
pub fn materialize_execution_segments(
    candidate: &crate::ExecutablePlanCandidateV1,
) -> Vec<ExecutionSegmentV1> {
    let mut segments = Vec::<ExecutionSegmentV1>::new();
    for step in &candidate.task_plan.steps {
        let mode = step.effective_mode();
        let isolation = step.effective_isolation();
        let authority_fingerprint = execution_segment_authority_fingerprint(candidate, step);
        let joins_previous = segments.last().is_some_and(|segment| {
            let Some(previous) = segment.ordered_step_ids.last() else {
                return false;
            };
            step.depends_on.as_slice() == std::slice::from_ref(previous)
                && segment.role == step.role
                && segment.isolation == isolation
                && segment.authority_fingerprint == authority_fingerprint
        });
        if joins_previous {
            if let Some(segment) = segments.last_mut() {
                segment.ordered_step_ids.push(step.step_id.clone());
            }
            continue;
        }
        let plan_version = candidate.task_plan.plan_version.to_string();
        let segment_id = format!(
            "segment-{}",
            &task_domain_hash(
                "sigil-execution-segment-v1",
                &[
                    candidate.task_id.as_str(),
                    &plan_version,
                    step.step_id.as_str(),
                    step.role.as_str(),
                    mode.as_str(),
                    isolation.as_str(),
                    &authority_fingerprint,
                ],
            )[..24]
        );
        segments.push(ExecutionSegmentV1 {
            segment_id,
            task_id: candidate.task_id.clone(),
            plan_version: candidate.task_plan.plan_version,
            ordered_step_ids: vec![step.step_id.clone()],
            role: step.role,
            authority_fingerprint,
            isolation,
            continuation_contract: ContinuationContractV1::ExactLinearSameAuthority,
            checkpoint_policy: SegmentCheckpointPolicyV1::EveryProviderTurn,
        });
    }
    segments
}

fn execution_segment_authority_fingerprint(
    candidate: &crate::ExecutablePlanCandidateV1,
    step: &TaskStepSpec,
) -> String {
    let mut capabilities = candidate
        .step_contracts
        .iter()
        .find(|binding| binding.step_id == step.step_id)
        .map(|binding| {
            binding
                .contract
                .required_capabilities
                .iter()
                .map(|capability| capability.as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    capabilities.sort_unstable();
    format!(
        "sha256:{}",
        task_domain_hash(
            "sigil-execution-segment-authority-v1",
            &[
                step.role.as_str(),
                step.effective_mode().as_str(),
                step.effective_isolation().as_str(),
                &capabilities.join(","),
            ],
        )
    )
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
#[serde(rename_all = "snake_case", deny_unknown_fields)]
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
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskStepContractBoundEntryV2 {
    pub task_id: TaskId,
    pub plan_version: u32,
    pub step_id: TaskStepId,
    pub contract: TaskStepContractV2,
}

/// Terminal marker proving that one accepted plan and its complete V2 sidecar set were committed
/// as a single recovery unit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
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

/// Host-proven availability of private worktree execution for one planner run.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskPlannerWorktreeAvailability {
    AvailableWithInteractiveReview,
    UnavailableHeadless,
    UnavailableWorkspace,
    #[default]
    UnavailableRunner,
}

impl TaskPlannerWorktreeAvailability {
    #[must_use]
    pub fn is_available(self) -> bool {
        self == Self::AvailableWithInteractiveReview
    }

    #[must_use]
    pub fn planner_material(self) -> &'static str {
        match self {
            Self::AvailableWithInteractiveReview => {
                "available: this workspace supports private Git worktrees and this interactive run can present the required integration review and promotion. Use worktree only when parallel physical isolation materially benefits the objective."
            }
            Self::UnavailableHeadless => {
                "unavailable: this headless run cannot complete the required integration review and promotion. Use executor with sequential_workspace_write for implementation changes."
            }
            Self::UnavailableWorkspace => {
                "unavailable: the host did not prove this workspace can materialize private Git worktrees. Use executor with sequential_workspace_write for implementation changes."
            }
            Self::UnavailableRunner => {
                "unavailable: this runtime cannot materialize and integrate private worktrees. Use executor with sequential_workspace_write for implementation changes."
            }
        }
    }
}

/// Bound task context for the internal planner tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskPlanUpdateContext {
    pub task_id: TaskId,
    pub max_plan_steps: usize,
    pub max_plan_versions: usize,
    #[serde(default)]
    pub worktree_availability: TaskPlannerWorktreeAvailability,
}

/// Host-owned facts exposed to one model-driven task-guidance review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskGuidanceAssessmentContext {
    pub queue_id: crate::ConversationInputQueueId,
    pub task_id: TaskId,
    pub plan_version: u32,
    pub dispatch_run_id: String,
    pub accepted_plan: TaskPlanEntry,
    pub eligible_pending_step_ids: Vec<TaskStepId>,
}

impl TaskGuidanceAssessmentContext {
    /// Validates that the review context is bound to one current accepted plan.
    pub fn validate_shape(&self) -> Result<()> {
        if self.plan_version == 0 {
            bail!("task guidance assessment plan version must be non-zero");
        }
        validate_stable_id(
            "task guidance assessment dispatch run id",
            &self.dispatch_run_id,
        )?;
        if self.accepted_plan.task_id != self.task_id
            || self.accepted_plan.plan_version != self.plan_version
            || self.accepted_plan.status != TaskPlanStatus::Accepted
        {
            bail!("task guidance assessment accepted plan does not match its task binding");
        }
        validate_task_plan_graph_steps(&self.accepted_plan.steps)?;
        let plan_step_ids = self
            .accepted_plan
            .steps
            .iter()
            .map(|step| &step.step_id)
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        for step_id in &self.eligible_pending_step_ids {
            if !plan_step_ids.contains(step_id) {
                bail!(
                    "task guidance assessment eligible step {} is absent from the accepted plan",
                    step_id.as_str()
                );
            }
            if !seen.insert(step_id) {
                bail!(
                    "task guidance assessment repeats eligible step {}",
                    step_id.as_str()
                );
            }
        }
        Ok(())
    }
}

/// Bounded model-owned reason for applying guidance without changing the accepted plan.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskGuidanceApplyReason {
    ClarifiesExistingStep,
    PrioritizesPendingStep,
    AddsExecutionConstraint,
}

/// Durable model decision that guidance can be materialized only into not-yet-started steps.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskGuidanceAppliedEntry {
    pub queue_id: crate::ConversationInputQueueId,
    pub task_id: TaskId,
    pub plan_version: u32,
    pub dispatch_run_id: String,
    pub reason: TaskGuidanceApplyReason,
    pub target_step_ids: Vec<TaskStepId>,
}

impl TaskGuidanceAppliedEntry {
    pub fn validate_against(&self, context: &TaskGuidanceAssessmentContext) -> Result<()> {
        context.validate_shape()?;
        if self.queue_id != context.queue_id
            || self.task_id != context.task_id
            || self.plan_version != context.plan_version
            || self.dispatch_run_id != context.dispatch_run_id
        {
            bail!("task guidance applied entry does not match its host assessment binding");
        }
        if self.target_step_ids.is_empty() {
            bail!("task guidance apply decision requires at least one target step");
        }
        let eligible = context
            .eligible_pending_step_ids
            .iter()
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        for step_id in &self.target_step_ids {
            if !eligible.contains(step_id) {
                bail!(
                    "task guidance apply target {} is not an eligible pending step",
                    step_id.as_str()
                );
            }
            if !seen.insert(step_id) {
                bail!(
                    "task guidance apply decision repeats target step {}",
                    step_id.as_str()
                );
            }
        }
        Ok(())
    }
}

/// Recovery-critical safe materialization of one accepted guidance supplement.
///
/// This record is appended atomically with the parent `TaskGuidanceApplied` decision after the
/// planner attempt is durably terminal. Non-sensitive guidance can therefore resume without
/// rerunning the planner. Sensitive guidance records only its safe projection and is explicitly
/// stale after process loss because the exact prompt is intentionally not persisted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskGuidanceMaterializedEntry {
    pub materialization_id: String,
    pub queue_id: crate::ConversationInputQueueId,
    pub task_id: TaskId,
    pub plan_version: u32,
    pub dispatch_run_id: String,
    pub prompt_hash: String,
    pub exact_prompt_required: bool,
    pub guidance: String,
    pub target_step_ids: Vec<TaskStepId>,
}

impl TaskGuidanceMaterializedEntry {
    pub fn new(
        applied: &TaskGuidanceAppliedEntry,
        prompt_hash: String,
        exact_prompt_required: bool,
        guidance: String,
    ) -> Result<Self> {
        let materialization_id = task_guidance_materialization_id(
            &applied.queue_id,
            &applied.task_id,
            applied.plan_version,
            &applied.dispatch_run_id,
        );
        let entry = Self {
            materialization_id,
            queue_id: applied.queue_id.clone(),
            task_id: applied.task_id.clone(),
            plan_version: applied.plan_version,
            dispatch_run_id: applied.dispatch_run_id.clone(),
            prompt_hash,
            exact_prompt_required,
            guidance,
            target_step_ids: applied.target_step_ids.clone(),
        };
        entry.validate_against(applied)?;
        Ok(entry)
    }

    pub fn validate_shape(&self) -> Result<()> {
        TaskId::new(self.task_id.as_str())?;
        if self.plan_version == 0 || self.dispatch_run_id.trim().is_empty() {
            bail!("task guidance materialization binding is incomplete");
        }
        if self.materialization_id
            != task_guidance_materialization_id(
                &self.queue_id,
                &self.task_id,
                self.plan_version,
                &self.dispatch_run_id,
            )
        {
            bail!("task guidance materialization identity does not match its binding");
        }
        if self.target_step_ids.is_empty() {
            bail!("task guidance materialization has no target steps");
        }
        let mut targets = BTreeSet::new();
        if self
            .target_step_ids
            .iter()
            .any(|step_id| !targets.insert(step_id))
        {
            bail!("task guidance materialization repeats a target step");
        }
        let projected = crate::project_conversation_prompt_for_persistence(&self.guidance);
        if projected.exact_prompt_required || projected.safe_prompt != self.guidance {
            bail!("task guidance materialization is not a safe durable projection");
        }
        let safe_hash = projected
            .prompt_hash
            .strip_prefix("safe:")
            .ok_or_else(|| anyhow!("task guidance materialization hash projection is invalid"))?;
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
            bail!("task guidance materialization does not match its prompt hash");
        }
        Ok(())
    }

    pub fn validate_against(&self, applied: &TaskGuidanceAppliedEntry) -> Result<()> {
        self.validate_shape()?;
        if self.queue_id != applied.queue_id
            || self.task_id != applied.task_id
            || self.plan_version != applied.plan_version
            || self.dispatch_run_id != applied.dispatch_run_id
            || self.target_step_ids != applied.target_step_ids
        {
            bail!("task guidance materialization does not match its applied decision");
        }
        Ok(())
    }
}

fn task_guidance_materialization_id(
    queue_id: &crate::ConversationInputQueueId,
    task_id: &TaskId,
    plan_version: u32,
    dispatch_run_id: &str,
) -> String {
    crate::stable_event_uuid(
        TASK_GUIDANCE_MATERIALIZATION_DOMAIN,
        &format!(
            "{}\n{}\n{plan_version}\n{dispatch_run_id}",
            queue_id.as_str(),
            task_id.as_str()
        ),
    )
}

/// Model-visible tool for guidance that does not require a plan or scope change.
pub fn task_guidance_apply_tool_spec() -> ToolSpec {
    ToolSpec {
        name: TASK_GUIDANCE_APPLY_TOOL_NAME.to_owned(),
        description: "Apply the user's guidance only when it clarifies, prioritizes, or constrains work already represented by not-yet-started steps in the accepted plan. If the guidance changes scope, accepted intent, dependencies, roles, isolation, or required steps, call task_plan_update with the next plan version instead."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "enum": [
                        "clarifies_existing_step",
                        "prioritizes_pending_step",
                        "adds_execution_constraint"
                    ]
                },
                "target_step_ids": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "string",
                        "minLength": 1
                    }
                }
            },
            "required": ["reason", "target_step_ids"],
            "additionalProperties": false
        }),
        category: ToolCategory::Custom,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct RawTaskGuidanceApplyArgs {
    reason: TaskGuidanceApplyReason,
    target_step_ids: Vec<TaskStepId>,
}

/// Parses one model decision to supplement the current accepted plan.
pub fn task_guidance_applied_entry(
    context: &TaskGuidanceAssessmentContext,
    call: &ToolCall,
) -> Result<TaskGuidanceAppliedEntry> {
    context.validate_shape()?;
    if call.name != TASK_GUIDANCE_APPLY_TOOL_NAME {
        bail!("unexpected internal task guidance tool {}", call.name);
    }
    let args: RawTaskGuidanceApplyArgs = serde_json::from_str(&call.args_json)
        .map_err(|error| anyhow!("invalid task guidance apply arguments: {error}"))?;
    let entry = TaskGuidanceAppliedEntry {
        queue_id: context.queue_id.clone(),
        task_id: context.task_id.clone(),
        plan_version: context.plan_version,
        dispatch_run_id: context.dispatch_run_id.clone(),
        reason: args.reason,
        target_step_ids: args.target_step_ids,
    };
    entry.validate_against(context)?;
    Ok(entry)
}

/// Bounded model-visible acknowledgement for an accepted supplement decision.
pub fn task_guidance_apply_result_content(entry: &TaskGuidanceAppliedEntry) -> String {
    json!({
        "task_id": entry.task_id.as_str(),
        "plan_version": entry.plan_version,
        "decision": "supplement_pending_steps",
        "target_steps": entry.target_step_ids.len(),
        "next_action": "stop; the system orchestrator will materialize the guidance into pending step inputs"
    })
    .to_string()
}

/// Model-visible schema for the internal planner plan-update tool.
pub fn task_plan_update_tool_spec() -> ToolSpec {
    task_plan_update_tool_spec_for_worktree(
        TaskPlannerWorktreeAvailability::AvailableWithInteractiveReview,
    )
}

pub(crate) fn task_plan_update_tool_spec_for_worktree(
    worktree_availability: TaskPlannerWorktreeAvailability,
) -> ToolSpec {
    let mut isolation_modes = vec![
        "shared_read_only",
        "sequential_workspace_write",
        "changeset_only",
    ];
    if worktree_availability.is_available() {
        isolation_modes.push("worktree");
    }
    ToolSpec {
        name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
        description: format!(
            "Create or replace the current durable task plan. Use this before executing task steps. Do not call task, subagent, or other delegation tools. Repository targets must be grounded in explicit objective paths or completed planner discovery; never guess files or report artifacts. Paths in step details must be relative to the bound workspace root and must not begin with a slash. Normalize a presentation-only leading slash from discovery prose, but never use an absolute host path. Use executor for ordinary main-session reads and edits. Use subagent_read only for delegated read-only investigation or advisory review. Verification checks are system-owned and must not be represented as participant steps. changeset_only is proposal-only and pauses for manual merge review. Use subagent_write with worktree isolation only when the host capability allows it. Worktree planning capability: {}",
            worktree_availability.planner_material()
        ),
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
                            "detail": {
                                "type": "string",
                                "description": "Bounded execution instructions. Any repository path must be workspace-relative and must not begin with a slash."
                            },
                            "target_paths": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "Concrete workspace-relative files or directories this step may inspect or change."
                            },
                            "required_capabilities": {
                                "type": "array",
                                "items": {
                                    "type": "string",
                                    "enum": ["workspace_read", "workspace_write", "vcs_read", "process_execute", "network_read", "artifact_read"]
                                },
                                "description": "Semantic participant capabilities required before the step may launch. Use vcs_read for git status/diff inspection instead of assuming shell access. Trusted verification is host-owned and must be expressed through check_spec_refs, not verification_run."
                            },
                            "deliverables": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "Concrete outputs this step must return to its dependents."
                            },
                            "acceptance_criteria": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "Observable completion criteria for this step."
                            },
                            "check_spec_refs": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "Trusted verification check identifiers consumed by the host verifier."
                            },
                            "risk": {"type": "string"},
                            "notes": {
                                "type": "array",
                                "items": {"type": "string"}
                            },
                            "role": {
                                "type": "string",
                                "enum": ["planner", "executor", "subagent_read", "subagent_write"],
                                "description": "Use executor for ordinary main-session work, including sequential_workspace_write edits. Use subagent_read for delegated read-only investigation or advisory review. changeset_only is proposal-only and pauses for manual merge review. Use subagent_write with worktree for a physically isolated writer that must implement and integrate changes."
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
                                "enum": ["read", "write", "review"],
                                "description": "Optional participant intent. Omit when the role default is enough. Reviewer output is advisory; system verification is not a participant step."
                            },
                            "isolation": {
                                "type": "string",
                                "enum": isolation_modes,
                                "description": format!("Optional workspace isolation contract. Omit unless a non-default is required. Write steps default to sequential_workspace_write for executor. subagent_write requires an advertised child-write isolation. changeset_only produces a proposal and pauses for manual merge review. Read/review steps always use shared_read_only. Worktree planning capability: {}", worktree_availability.planner_material())
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
    Ok(task_plan_update_commit_v2(context, call)?.plan)
}

/// Parsed task plan plus its lossless V2 execution-contract sidecars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPlanUpdateCommitV2 {
    pub plan: TaskPlanEntry,
    pub step_contracts: Vec<TaskStepContractBoundEntryV2>,
}

/// Parses one internal `task_plan_update` call without dropping step execution metadata.
///
/// # Errors
///
/// Returns an error when the plan or any V2 sidecar is invalid.
pub fn task_plan_update_commit_v2(
    context: &TaskPlanUpdateContext,
    call: &ToolCall,
) -> Result<TaskPlanUpdateCommitV2> {
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
    let plan_version = args.plan_version;
    let status = args.status;
    let reason = args.reason;
    let mut steps = Vec::with_capacity(args.steps.len());
    let mut step_contracts = Vec::with_capacity(args.steps.len());
    for step in args.steps {
        let raw_step_id = step.step_id.clone();
        let display_name = match step.display_name.as_deref() {
            Some(display_name) => {
                let normalized =
                    normalize_task_agent_display_name(display_name).map_err(|error| {
                        anyhow!("invalid display_name for step {}: {error}", step.step_id)
                    })?;
                Some(
                    normalize_task_agent_display_name(&crate::safe_persistence_text(&normalized))
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
        let step_id = TaskStepId::new(step.step_id)?;
        let task_step = TaskStepSpec {
            step_id: step_id.clone(),
            title: crate::safe_persistence_text(&step.title),
            display_name,
            detail: step.detail.as_deref().map(crate::safe_persistence_text),
            role: step.role,
            depends_on: step
                .depends_on
                .into_iter()
                .map(TaskStepId::new)
                .collect::<Result<Vec<_>>>()?,
            intent_refs: Vec::new(),
            mode: Some(mode),
            isolation: Some(isolation),
        };
        if step
            .required_capabilities
            .contains(&TaskCapabilityV2::VerificationRun)
        {
            bail!(
                "task planner cannot delegate verification_run for step {raw_step_id}; use check_spec_refs for host-owned verification"
            );
        }
        let mut required_capabilities = default_task_step_capabilities(mode, isolation)
            .into_iter()
            .collect::<BTreeSet<_>>();
        required_capabilities.extend(step.required_capabilities);
        let contract = TaskStepContractV2 {
            schema_version: TASK_STEP_CONTRACT_V2_SCHEMA_VERSION,
            target_paths: step.target_paths,
            required_capabilities: required_capabilities.into_iter().collect(),
            deliverables: step.deliverables,
            acceptance_criteria: step.acceptance_criteria,
            check_spec_refs: step.check_spec_refs,
            risk: step.risk,
            notes: step.notes,
        };
        contract.validate().map_err(|error| {
            anyhow!("invalid execution contract for step {raw_step_id}: {error}")
        })?;
        steps.push(task_step);
        step_contracts.push(TaskStepContractBoundEntryV2 {
            task_id: context.task_id.clone(),
            plan_version,
            step_id,
            contract,
        });
    }
    validate_task_plan_graph_steps(&steps)?;
    if steps
        .iter()
        .any(|step| step.effective_mode() == TaskStepMode::Verify)
    {
        bail!(
            "task planner cannot create verify participant steps; trusted verification is system-owned"
        );
    }
    if !context.worktree_availability.is_available()
        && steps
            .iter()
            .any(|step| step.effective_isolation() == TaskIsolationMode::Worktree)
    {
        bail!(
            "worktree isolation is unavailable for this planning run; use executor with sequential_workspace_write"
        );
    }
    Ok(TaskPlanUpdateCommitV2 {
        plan: TaskPlanEntry {
            task_id: context.task_id.clone(),
            plan_version,
            status,
            steps,
            reason: reason.as_deref().map(crate::safe_persistence_text),
        },
        step_contracts,
    })
}

fn default_task_step_capabilities(
    mode: TaskStepMode,
    isolation: TaskIsolationMode,
) -> Vec<TaskCapabilityV2> {
    match mode {
        TaskStepMode::Write if isolation == TaskIsolationMode::ChangesetOnly => {
            vec![TaskCapabilityV2::WorkspaceRead]
        }
        TaskStepMode::Write => vec![
            TaskCapabilityV2::WorkspaceRead,
            TaskCapabilityV2::WorkspaceWrite,
        ],
        TaskStepMode::Read | TaskStepMode::Review => vec![TaskCapabilityV2::WorkspaceRead],
        TaskStepMode::Verify => vec![TaskCapabilityV2::VerificationRun],
    }
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
    #[serde(default)]
    pub target_paths: Vec<String>,
    #[serde(default)]
    pub required_capabilities: Vec<TaskCapabilityV2>,
    #[serde(default)]
    pub deliverables: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub check_spec_refs: Vec<String>,
    #[serde(default)]
    pub risk: Option<String>,
    #[serde(default)]
    pub notes: Vec<String>,
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
#[serde(rename_all = "snake_case", deny_unknown_fields)]
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
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum TaskExecutionBindingV1 {
    /// One accepted multi-step TaskPlan generation.
    Plan { plan_version: u32 },
    /// One first-class direct-execution admission.
    Direct { admission_id: String },
}

impl TaskExecutionBindingV1 {
    fn validate(&self) -> bool {
        match self {
            Self::Plan { plan_version } => *plan_version > 0,
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
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskPauseRequest {
    pub request_id: String,
    pub task_id: TaskId,
    pub execution: TaskExecutionBindingV1,
}

impl TaskPauseRequest {
    #[must_use]
    pub fn new(task_id: TaskId, plan_version: u32) -> Self {
        let mut request = Self {
            request_id: String::new(),
            task_id,
            execution: TaskExecutionBindingV1::Plan { plan_version },
        };
        request.request_id = request.expected_request_id();
        request
    }

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
        if self.purpose != TaskParticipantPurpose::Step && self.child_session_ref != expected_ref {
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
    let predecessor_id = if let Some(segments) = task.execution_segments.get(&plan_version) {
        let segment = segments
            .iter()
            .find(|segment| segment.ordered_step_ids.contains(&step.step_id))?;
        let position = segment
            .ordered_step_ids
            .iter()
            .position(|step_id| step_id == &step.step_id)?;
        segment
            .ordered_step_ids
            .get(position.checked_sub(1)?)?
            .clone()
    } else {
        // Legacy materialization records did not carry an execution-segment receipt. Their
        // existing deterministic plan projection remains readable, but new records always take
        // the authoritative branch above.
        let segment = derive_task_execution_segments(&plan.steps)
            .into_iter()
            .find(|segment| segment.step_ids.contains(&step.step_id))?;
        let position = segment
            .step_ids
            .iter()
            .position(|step_id| step_id == &step.step_id)?;
        segment.step_ids.get(position.checked_sub(1)?)?.clone()
    };
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

/// Durable proof that a bounded participant retry is safe for its declared recovery class.
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
    /// A read-only participant's latest physical attempt ended after provider output was
    /// observed, but the attempt produced no external side effect. The replacement is a new
    /// durable participant attempt using the exact same task input; it is not a transparent
    /// replay of the failed physical request.
    ProviderProtocolRejectedAfterOutput {
        physical_attempt_id: String,
        request_material_fingerprint: String,
        read_only_step: bool,
        zero_effect: bool,
    },
}

impl TaskParticipantRetryProof {
    /// Validates the explicit safety facts for the selected recovery class and the referenced
    /// provider evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when a required safety fact is false or an evidence fingerprint is
    /// invalid.
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
            Self::ProviderProtocolRejectedAfterOutput {
                physical_attempt_id,
                request_material_fingerprint,
                read_only_step,
                zero_effect,
            } => {
                validate_stable_id("provider physical attempt id", physical_attempt_id)?;
                validate_prefixed_sha256(
                    "provider request material fingerprint",
                    request_material_fingerprint,
                    "hmac-sha256:",
                )?;
                if !read_only_step || !zero_effect {
                    bail!(
                        "provider protocol recovery proof must establish a read-only step and zero effect"
                    );
                }
                return Ok(());
            }
        };
        if !zero_output || !zero_tool || !zero_effect {
            bail!("task participant retry proof must establish zero output, tool, and effect");
        }
        Ok(())
    }

    #[must_use]
    pub fn recovery_label(&self) -> &'static str {
        match self {
            Self::ProviderConfirmedNoConsumption { .. }
            | Self::AdmissionRejectedBeforeDispatch { .. } => "provider pressure",
            Self::ProviderProtocolRejectedAfterOutput { .. } => "provider protocol recovery",
        }
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

/// Durable, hash-only checkpoint for one task-participant model turn.
///
/// The entry intentionally stores neither tool arguments nor tool output. It lets recovery and
/// the live agent loop distinguish useful frontier movement from an exact repeated analysis batch
/// without copying potentially sensitive child-session content into control state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskStepCheckpointV2 {
    pub schema_version: u16,
    pub task_id: TaskId,
    pub plan_version: u32,
    pub step_id: TaskStepId,
    pub attempt_id: TaskParticipantAttemptId,
    pub model_turn: u32,
    pub semantic_call_hash: String,
    pub result_frontier_hash: String,
    pub no_progress_count: u32,
}

impl TaskStepCheckpointV2 {
    /// Validates the bounded checkpoint identity and hash-only frontier.
    ///
    /// # Errors
    ///
    /// Returns an error when the schema, task binding, turn, or digest is invalid.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != TASK_STEP_CONTRACT_V2_SCHEMA_VERSION {
            bail!(
                "unsupported task step checkpoint schema version {}",
                self.schema_version
            );
        }
        if self.plan_version == 0 || self.model_turn == 0 {
            bail!("task step checkpoint is missing its plan or model-turn identity");
        }
        TaskId::new(self.task_id.as_str())?;
        TaskStepId::new(self.step_id.as_str())?;
        TaskParticipantAttemptId::new(self.attempt_id.as_str())?;
        validate_sha256_fingerprint("task semantic call hash", &self.semantic_call_hash)?;
        validate_sha256_fingerprint("task result frontier hash", &self.result_frontier_hash)?;
        Ok(())
    }

    #[must_use]
    pub fn repeated_frontier(&self, previous: Option<&Self>) -> bool {
        previous.is_some_and(|previous| {
            previous.task_id == self.task_id
                && previous.plan_version == self.plan_version
                && previous.step_id == self.step_id
                && previous.attempt_id == self.attempt_id
                && previous.semantic_call_hash == self.semantic_call_hash
                && previous.result_frontier_hash == self.result_frontier_hash
        })
    }
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
    /// Monotonic RFC-0067 admission attempts per Task.
    pub admission_attempts: BTreeMap<TaskId, Vec<TaskAdmissionAttemptV1>>,
    /// Latest unresolved RFC-0067 blocker per Task.
    pub active_blockers: BTreeMap<TaskId, TaskBlockerV1>,
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
            ControlEntry::TaskCreatedFromPlan(entry) if entry.stale_reason.is_none() => {
                self.select_current_task(&entry.task_id);
            }
            ControlEntry::PlanExecutionAdoptedV1(adoption) => {
                self.apply_adoption(adoption);
            }
            ControlEntry::TaskMaterializationAttemptStartedV1(attempt) => {
                self.apply_materialization_attempt(attempt);
            }
            ControlEntry::TaskMaterializationPreparedV1(materialization) => {
                self.apply_materialization(materialization);
            }
            ControlEntry::TaskMaterializationBlockedV1(blocked) => {
                self.apply_materialization_blocked(blocked);
            }
            ControlEntry::TaskAdmissionAttemptedV1(attempt) => {
                self.apply_admission_attempt(attempt);
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
            ControlEntry::TaskChecklistUpdatedV1(entry) => self.apply_checklist(entry),
            ControlEntry::TaskPlan(entry) => self.apply_plan(entry),
            ControlEntry::TaskStepContractBoundV2(entry) => self.apply_step_contract(entry),
            ControlEntry::TaskPlanContractSetCommittedV2(entry) => {
                self.apply_contract_set_commit(entry)
            }
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

    fn apply_continuation_focus(&mut self, entry: &crate::TaskContinuationSelectedEntry) {
        if entry.validate_shape().is_err() {
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
        ) || task
            .plans
            .get(&entry.plan_version)
            .is_none_or(|plan| plan.status != TaskPlanStatus::Accepted)
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
        self.record_task_replay(
            &entry.task_id,
            task_is_new && entry.status == TaskRunStatus::Started,
        );
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
                step_contracts: BTreeMap::new(),
                contract_set_committed_v2: false,
                graph,
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

    /// Derives the RFC-0067 Task state from the single adoption authority.
    ///
    /// No separate TaskRun/TaskPlan/contract records are required; every existing view is
    /// synthesized from the adopted candidate so the Task identity exists atomically with the
    /// accepted plan, step contracts and intent lineage.
    fn apply_adoption(&mut self, adoption: &crate::PlanExecutionAdoptedV1Entry) {
        self.apply_materialized_candidate(adoption, true);
    }

    /// Applies RFC-0069 post-approval materialization to an already durable Task shell.
    fn apply_materialization(&mut self, materialization: &crate::PlanExecutionAdoptedV1Entry) {
        self.apply_materialized_candidate(materialization, false);
        self.active_blockers.remove(&materialization.task_id);
    }

    fn apply_materialization_attempt(
        &mut self,
        attempt: &crate::TaskMaterializationAttemptStartedV1,
    ) {
        if attempt.validate().is_err() || !self.tasks.contains_key(&attempt.task_id) {
            return;
        }
        self.record_task_replay(&attempt.task_id, false);
    }

    fn apply_materialization_blocked(&mut self, blocked: &crate::TaskMaterializationBlockedV1) {
        if blocked.validate().is_err() || !self.tasks.contains_key(&blocked.task_id) {
            return;
        }
        self.record_task_replay(&blocked.task_id, false);
        self.active_blockers
            .insert(blocked.task_id.clone(), blocked.blocker.clone());
    }

    fn apply_materialized_candidate(
        &mut self,
        adoption: &crate::PlanExecutionAdoptedV1Entry,
        allow_create_shell: bool,
    ) {
        let candidate = &adoption.adopted_candidate;
        let desired_status = if adoption.start_mode == crate::PlanTaskStartMode::CreatePaused {
            TaskRunStatus::Paused
        } else {
            TaskRunStatus::Started
        };
        if allow_create_shell && !self.tasks.contains_key(&candidate.task_id) {
            self.apply_run(&TaskRunEntry {
                task_id: candidate.task_id.clone(),
                parent_session_ref: adoption.parent_session_ref.clone(),
                objective: candidate.safe_objective.clone(),
                title: Some(candidate.semantic_title.clone()),
                status: desired_status,
                reason: Some(format!("adopted from plan {}", adoption.plan_id.as_str())),
            });
        }
        if !self.tasks.contains_key(&candidate.task_id) {
            // A new materialization record may never manufacture a Task shell. Keep replay
            // fail-closed; the durable approval bundle is the only shell authority.
            return;
        }
        if self
            .tasks
            .get(&candidate.task_id)
            .and_then(|task| task.plans.get(&candidate.task_plan.plan_version))
            .is_none()
        {
            self.apply_plan(&candidate.task_plan.clone());
            for contract in &candidate.step_contracts {
                self.apply_step_contract(contract);
            }
            self.apply_contract_set_commit(&TaskPlanContractSetCommittedV2 {
                schema_version: TASK_STEP_CONTRACT_V2_SCHEMA_VERSION,
                task_id: candidate.task_id.clone(),
                plan_version: candidate.task_plan.plan_version,
                contract_count: candidate.step_contracts.len(),
                contract_set_sha256: candidate.contract_set_digest.clone(),
            });
        }
        if self.tasks.contains_key(&candidate.task_id) {
            if let Some(segments) = adoption.execution_segments.as_ref() {
                self.tasks
                    .entry(candidate.task_id.clone())
                    .or_insert_with(|| TaskRunProjection::placeholder(candidate.task_id.clone()))
                    .execution_segments
                    .insert(candidate.task_plan.plan_version, segments.clone());
            }
            self.select_current_task(&candidate.task_id);
        }
    }

    fn apply_admission_attempt(&mut self, attempt: &TaskAdmissionAttemptV1) {
        self.admission_attempts
            .entry(attempt.task_id.clone())
            .or_default()
            .push(attempt.clone());
        match &attempt.outcome {
            TaskAdmissionOutcomeV1::Blocked(blocker) => {
                self.active_blockers
                    .insert(attempt.task_id.clone(), blocker.clone());
            }
            TaskAdmissionOutcomeV1::Ready(_) | TaskAdmissionOutcomeV1::Paused(_) => {
                self.active_blockers.remove(&attempt.task_id);
            }
        }
    }

    /// Returns the latest RFC-0067 admission attempt for one Task, if any.
    pub fn latest_admission_attempt(&self, task_id: &TaskId) -> Option<&TaskAdmissionAttemptV1> {
        self.admission_attempts
            .get(task_id)
            .and_then(|attempts| attempts.last())
    }

    /// Returns the next monotonic admission ordinal for one Task.
    #[must_use]
    pub fn next_admission_ordinal(&self, task_id: &TaskId) -> u32 {
        self.latest_admission_attempt(task_id)
            .map_or(0, |attempt| attempt.ordinal)
            .saturating_add(1)
    }

    /// Returns the latest unresolved blocker for one Task, if any.
    pub fn active_blocker(&self, task_id: &TaskId) -> Option<&TaskBlockerV1> {
        self.active_blockers.get(task_id)
    }

    /// Derives the RFC-0067 execution phase from durable facts only.
    pub fn execution_phase(&self, task_id: &TaskId) -> Option<TaskExecutionPhaseV1> {
        let task = self.tasks.get(task_id)?;
        let phase = match task.status {
            TaskRunStatus::Completed => TaskExecutionPhaseV1::Completed,
            TaskRunStatus::Failed => TaskExecutionPhaseV1::Failed,
            TaskRunStatus::Cancelled => TaskExecutionPhaseV1::Cancelled,
            TaskRunStatus::Interrupted => TaskExecutionPhaseV1::Interrupted,
            TaskRunStatus::Paused => TaskExecutionPhaseV1::Paused,
            TaskRunStatus::Running => TaskExecutionPhaseV1::Running,
            TaskRunStatus::Started => match self.latest_admission_attempt(task_id) {
                Some(attempt) => match &attempt.outcome {
                    TaskAdmissionOutcomeV1::Ready(_) => TaskExecutionPhaseV1::Ready,
                    TaskAdmissionOutcomeV1::Blocked(_) => TaskExecutionPhaseV1::Blocked,
                    TaskAdmissionOutcomeV1::Paused(_) => TaskExecutionPhaseV1::Paused,
                },
                None if task.direct_execution_admission.is_some() => TaskExecutionPhaseV1::Ready,
                None => TaskExecutionPhaseV1::Preparing,
            },
        };
        Some(phase)
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

    fn apply_participant_retry_scheduled(&mut self, entry: &TaskParticipantRetryScheduledEntry) {
        self.record_task_replay(&entry.task_id, false);
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

    fn apply_final_answer(&mut self, entry: &TaskFinalAnswerCommittedEntry) {
        self.record_task_replay(&entry.task_id, false);
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
    pub participant_retry_schedules:
        BTreeMap<TaskParticipantAttemptId, TaskParticipantRetryScheduledEntry>,
    pub participant_results: BTreeMap<TaskParticipantAttemptId, TaskParticipantResultEntry>,
    /// Authoritative RFC-0069 continuity receipts keyed by their accepted task-plan version.
    pub execution_segments: BTreeMap<u32, Vec<ExecutionSegmentV1>>,
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
            participant_retry_schedules: BTreeMap::new(),
            participant_results: BTreeMap::new(),
            execution_segments: BTreeMap::new(),
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
            participant_retry_schedules: BTreeMap::new(),
            participant_results: BTreeMap::new(),
            execution_segments: BTreeMap::new(),
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
    /// V2 sidecars keyed by step. Empty for legacy V1 plans.
    pub step_contracts: BTreeMap<TaskStepId, TaskStepContractV2>,
    /// True only after the exact complete V2 set commit marker replays successfully.
    pub contract_set_committed_v2: bool,
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
                        // A continuation is also the recovery lane for a step whose previous
                        // attempt ended in a recoverable block.  Keeping these states in the
                        // ready set lets the dependency graph re-evaluate downstream steps
                        // after the prerequisite succeeds, instead of requiring a new plan.
                        TaskStepStatus::Pending
                            | TaskStepStatus::Failed
                            | TaskStepStatus::Blocked
                            | TaskStepStatus::Cancelled
                            | TaskStepStatus::Interrupted
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
                changeset_only_batch: Vec::new(),
                worktree_batch: Vec::new(),
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
        let running_exclusive_write = running_steps.iter().any(|step| {
            !step.is_parallel_read_only()
                && !step.is_parallel_changeset_only()
                && !step.is_parallel_worktree()
        });
        if running_exclusive_write {
            return TaskReadyQueue {
                read_only_batch: Vec::new(),
                changeset_only_batch: Vec::new(),
                worktree_batch: Vec::new(),
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
        let running_changeset_only = running_steps
            .iter()
            .filter(|step| step.is_parallel_changeset_only())
            .count();
        let running_worktree = running_steps
            .iter()
            .filter(|step| step.is_parallel_worktree())
            .count();
        let read_only_capacity = options
            .max_concurrent_read_only
            .saturating_sub(running_read_only);
        let changeset_only_capacity = options
            .max_concurrent_changeset_only
            .saturating_sub(running_changeset_only);
        let worktree_capacity = options
            .max_concurrent_changeset_only
            .saturating_sub(running_worktree);
        let mut ready_read_only = Vec::new();
        let mut ready_changeset_only = Vec::new();
        let mut ready_worktree = Vec::new();
        let mut ready_write_steps = Vec::new();

        for step in ready_steps {
            if step.is_parallel_read_only() {
                ready_read_only.push(step);
            } else if step.is_parallel_changeset_only() {
                ready_changeset_only.push(step);
            } else if step.is_parallel_worktree() {
                ready_worktree.push(step);
            } else {
                ready_write_steps.push(step);
            }
        }

        let mut deferred = Vec::new();
        let mut read_only_batch = Vec::new();
        if running_changeset_only == 0 && running_worktree == 0 {
            for step in ready_read_only {
                if read_only_batch.len() < read_only_capacity {
                    read_only_batch.push(step.clone());
                } else {
                    deferred.push(TaskReadyDeferredStep {
                        step_id: step.step_id.clone(),
                        reason: TaskReadyDeferredReason::ConcurrencyBudget,
                    });
                }
            }
        } else {
            deferred.extend(
                ready_read_only
                    .into_iter()
                    .map(|step| TaskReadyDeferredStep {
                        step_id: step.step_id.clone(),
                        reason: if running_changeset_only > 0 {
                            TaskReadyDeferredReason::RunningChangesetOnly
                        } else {
                            TaskReadyDeferredReason::RunningWorktree
                        },
                    }),
            );
        }

        let may_start_changeset =
            read_only_batch.is_empty() && running_read_only == 0 && running_worktree == 0;
        let mut changeset_only_batch = Vec::new();
        if may_start_changeset {
            for step in ready_changeset_only {
                if changeset_only_batch.len() < changeset_only_capacity {
                    changeset_only_batch.push(step.clone());
                } else {
                    deferred.push(TaskReadyDeferredStep {
                        step_id: step.step_id.clone(),
                        reason: TaskReadyDeferredReason::ConcurrencyBudget,
                    });
                }
            }
        } else {
            deferred.extend(
                ready_changeset_only
                    .into_iter()
                    .map(|step| TaskReadyDeferredStep {
                        step_id: step.step_id.clone(),
                        reason: TaskReadyDeferredReason::RunningReadOnly,
                    }),
            );
        }

        let may_start_worktree = read_only_batch.is_empty()
            && changeset_only_batch.is_empty()
            && running_read_only == 0
            && running_changeset_only == 0;
        let mut worktree_batch = Vec::new();
        if may_start_worktree {
            for step in ready_worktree {
                if worktree_batch.len() < worktree_capacity {
                    worktree_batch.push(step.clone());
                } else {
                    deferred.push(TaskReadyDeferredStep {
                        step_id: step.step_id.clone(),
                        reason: TaskReadyDeferredReason::ConcurrencyBudget,
                    });
                }
            }
        } else {
            deferred.extend(
                ready_worktree
                    .into_iter()
                    .map(|step| TaskReadyDeferredStep {
                        step_id: step.step_id.clone(),
                        reason: if running_changeset_only > 0 || !changeset_only_batch.is_empty() {
                            TaskReadyDeferredReason::RunningChangesetOnly
                        } else {
                            TaskReadyDeferredReason::RunningReadOnly
                        },
                    }),
            );
        }

        let may_start_write = read_only_batch.is_empty()
            && changeset_only_batch.is_empty()
            && worktree_batch.is_empty()
            && running_read_only == 0
            && running_changeset_only == 0
            && running_worktree == 0;
        let mut sequential_step = None;
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
                } else if running_changeset_only > 0 || !changeset_only_batch.is_empty() {
                    TaskReadyDeferredReason::RunningChangesetOnly
                } else if running_worktree > 0 || !worktree_batch.is_empty() {
                    TaskReadyDeferredReason::RunningWorktree
                } else {
                    TaskReadyDeferredReason::SequentialWrite
                },
            });
        }

        TaskReadyQueue {
            read_only_batch,
            changeset_only_batch,
            worktree_batch,
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

    #[must_use]
    pub fn is_parallel_changeset_only(&self) -> bool {
        self.mode == TaskStepMode::Write && self.isolation == TaskIsolationMode::ChangesetOnly
    }

    #[must_use]
    pub fn is_parallel_worktree(&self) -> bool {
        self.mode == TaskStepMode::Write && self.isolation == TaskIsolationMode::Worktree
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskReadyQueueOptions {
    pub max_concurrent_read_only: usize,
    pub max_concurrent_changeset_only: usize,
}

impl TaskReadyQueueOptions {
    #[must_use]
    pub fn new(max_concurrent_read_only: usize) -> Self {
        Self {
            max_concurrent_read_only,
            max_concurrent_changeset_only: 1,
        }
    }

    #[must_use]
    pub fn with_max_concurrent_changeset_only(
        mut self,
        max_concurrent_changeset_only: usize,
    ) -> Self {
        self.max_concurrent_changeset_only = max_concurrent_changeset_only;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReadyQueue {
    pub read_only_batch: Vec<TaskGraphStepProjection>,
    pub changeset_only_batch: Vec<TaskGraphStepProjection>,
    pub worktree_batch: Vec<TaskGraphStepProjection>,
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
    RunningChangesetOnly,
    RunningWorktree,
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
