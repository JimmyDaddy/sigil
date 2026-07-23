use super::*;
use serde::{Deserialize, Serialize};

type TaskChildSessionBatchCommitAction = Box<
    dyn FnOnce(
            &mut Session,
            &mut (dyn EventHandler + Send),
        ) -> Result<Vec<Result<TaskChildSessionRunOutput>>>
        + Send,
>;

/// One-shot parent commit returned after a detached task batch has fully settled.
///
/// The action owns runtime-private completion material but cannot run until the kernel explicitly
/// gives it the parent session again. This keeps the parent borrow out of the participant future.
pub struct TaskChildSessionBatchCommitEnvelope {
    request_count: usize,
    commit: TaskChildSessionBatchCommitAction,
}

impl TaskChildSessionBatchCommitEnvelope {
    /// Creates a one-shot parent commit for an exact number of batch requests.
    pub fn new<F>(request_count: usize, commit: F) -> Self
    where
        F: FnOnce(
                &mut Session,
                &mut (dyn EventHandler + Send),
            ) -> Result<Vec<Result<TaskChildSessionRunOutput>>>
            + Send
            + 'static,
    {
        Self {
            request_count,
            commit: Box::new(commit),
        }
    }

    #[must_use]
    pub fn request_count(&self) -> usize {
        self.request_count
    }

    /// Applies the runtime-produced completion through the parent single writer exactly once.
    pub fn commit(
        self,
        parent_session: &mut Session,
        handler: &mut (dyn EventHandler + Send),
    ) -> Result<Vec<Result<TaskChildSessionRunOutput>>> {
        (self.commit)(parent_session, handler)
    }
}

impl std::fmt::Debug for TaskChildSessionBatchCommitEnvelope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TaskChildSessionBatchCommitEnvelope")
            .field("request_count", &self.request_count)
            .finish_non_exhaustive()
    }
}

/// Parent-free participant execution returned by a detached batch preparation.
pub type TaskChildSessionBatchFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<TaskChildSessionBatchCommitEnvelope>> + Send + 'a>,
>;

/// Explicit result of the synchronous batch preparation boundary.
pub enum TaskChildSessionBatchPreparation<'a> {
    /// Compatibility path for runners that still require the parent session across their await.
    Fallback(Vec<TaskChildSessionRunRequest>),
    /// Participant execution whose future cannot borrow the parent session.
    Detached(TaskChildSessionBatchFuture<'a>),
}

impl std::fmt::Debug for TaskChildSessionBatchPreparation<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fallback(requests) => formatter
                .debug_tuple("Fallback")
                .field(&requests.len())
                .finish(),
            Self::Detached(_) => formatter.write_str("Detached(..)"),
        }
    }
}

/// Typed runtime-to-orchestrator failure that carries the complete proof needed to schedule one
/// bounded provider-pressure retry.
#[derive(Debug, thiserror::Error)]
#[error("task participant was rate limited before output or effect: {source}")]
pub struct TaskParticipantRetryError {
    retry_after_ms: u64,
    route_fingerprint: String,
    input_hash: String,
    proof: TaskParticipantRetryProof,
    #[source]
    source: anyhow::Error,
}

impl TaskParticipantRetryError {
    /// Builds one retryable failure after runtime has proven that dispatch produced no observable
    /// model output, tool work, or external effect.
    ///
    /// # Errors
    ///
    /// Returns an error when scheduling metadata or proof is malformed.
    pub fn new(
        retry_after_ms: u64,
        route_fingerprint: impl Into<String>,
        input_hash: impl Into<String>,
        proof: TaskParticipantRetryProof,
        source: anyhow::Error,
    ) -> Result<Self> {
        let route_fingerprint = route_fingerprint.into();
        let input_hash = input_hash.into();
        if retry_after_ms == 0 || retry_after_ms > MAX_TASK_PARTICIPANT_AUTO_RETRY_WAIT_MS {
            bail!("task participant retry delay is outside the bounded automatic retry budget");
        }
        if route_fingerprint.len() != 71
            || !route_fingerprint.starts_with("sha256:")
            || !route_fingerprint[7..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("task participant retry route fingerprint is invalid");
        }
        if input_hash.len() != 64 || !input_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("task participant retry input hash is invalid");
        }
        proof.validate_shape()?;
        Ok(Self {
            retry_after_ms,
            route_fingerprint,
            input_hash,
            proof,
            source,
        })
    }

    #[must_use]
    pub fn retry_after_ms(&self) -> u64 {
        self.retry_after_ms
    }

    #[must_use]
    pub fn route_fingerprint(&self) -> &str {
        &self.route_fingerprint
    }

    #[must_use]
    pub fn input_hash(&self) -> &str {
        &self.input_hash
    }

    #[must_use]
    pub fn proof(&self) -> &TaskParticipantRetryProof {
        &self.proof
    }
}

/// Computes the retry-stable hash of task input material.
///
/// Attempt identity, cancellation handles, and logical-run ids are intentionally excluded so a
/// replacement physical attempt can prove that its user-visible request is unchanged.
///
/// # Errors
///
/// Returns an error when transient message persistence projection fails.
pub fn task_participant_input_hash(input: &AgentRunInput) -> Result<String> {
    let transient_context = input
        .transient_context
        .iter()
        .cloned()
        .map(crate::project_message_for_persistence)
        .map(|projection| {
            projection.map(|(mut durable, _overlay)| {
                // Message ids are local persistence identities, not provider-visible input.
                // Reconstructing the same task prompt for a new physical attempt creates a fresh
                // id, so retaining it here would make every safe retry look like input drift.
                durable.id.clear();
                durable
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let value = serde_json::json!({
        "persisted_user_message": input
            .persisted_user_message
            .as_deref()
            .map(crate::safe_persistence_text),
        "transient_context": transient_context,
        "task_plan_update": input.task_plan_update.as_ref().map(|context| {
            serde_json::json!({
                "task_id": context.task_id.as_str(),
                "max_plan_steps": context.max_plan_steps,
            })
        }),
    });
    let bytes = serde_json::to_vec(&value)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Request for one sequential planner/executor task run.
#[derive(Debug, Clone)]
pub struct SequentialTaskRequest {
    pub task_id: TaskId,
    pub parent_session_ref: SessionRef,
    pub objective: String,
}

/// Result of one sequential task run.
#[derive(Debug, Clone)]
pub struct SequentialTaskRunOutput {
    pub task_id: TaskId,
    pub plan_version: u32,
    pub steps: Vec<SequentialTaskStepOutput>,
    pub status: TaskRunStatus,
}

#[derive(Debug, Clone)]
pub struct SequentialTaskStepOutput {
    pub step_id: TaskStepId,
    pub status: TaskStepStatus,
    pub verification_verdict: VerificationVerdict,
    pub visible_state: VisibleCompletionState,
    pub outcome: AgentRunOutcome,
}

/// Exact projection binding required to rerun one trusted task verification check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskVerificationRerunRequest {
    pub task_id: TaskId,
    pub step_id: TaskStepId,
    pub check_spec_id: CheckSpecId,
    pub check_spec_hash: String,
    pub policy_hash: PolicyHash,
    pub workspace_snapshot_id: WorkspaceSnapshotId,
}

/// Durable terminal records produced by one exact task verification rerun.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskVerificationRerunOutput {
    pub check_run: VerificationCheckRunEntry,
    pub verification: VerificationRecordedEntry,
}

/// Input passed from the task orchestrator to a runtime-owned child-session runner.
#[derive(Debug, Clone)]
pub struct TaskChildSessionRunRequest {
    pub task: SequentialTaskRequest,
    pub attempt_id: TaskParticipantAttemptId,
    pub child_session_ref: SessionRef,
    pub plan_version: u32,
    pub step: TaskStepSpec,
    pub child_input: AgentRunInput,
    pub options: AgentRunOptions,
    pub changeset_only_base_snapshot_id: Option<String>,
}

/// Output returned by a child-session runner after a terminal child run.
#[derive(Debug, Clone)]
pub struct TaskChildSessionRunOutput {
    pub attempt_id: TaskParticipantAttemptId,
    pub final_text: String,
    pub outcome: AgentRunOutcome,
    pub child_session_ref: SessionRef,
    pub final_answer_ref: Option<AgentFinalAnswerRef>,
    pub artifact_refs: Vec<AgentArtifactRef>,
    pub changeset_proposal: Option<TaskChildChangeSetProposal>,
    pub changeset_only_after_snapshot_id: Option<String>,
}

/// Input for the isolated planner transcript owned by one durable participant attempt.
#[derive(Debug, Clone)]
pub struct TaskPlannerSessionRunRequest {
    pub task: SequentialTaskRequest,
    pub attempt_id: TaskParticipantAttemptId,
    pub child_session_ref: SessionRef,
    pub child_input: AgentRunInput,
    pub options: AgentRunOptions,
    pub discovery_options: AgentRunOptions,
}

/// Parent-committable output from an isolated planner transcript.
#[derive(Debug, Clone)]
pub struct TaskPlannerSessionRunOutput {
    pub attempt_id: TaskParticipantAttemptId,
    pub accepted_plan: TaskPlanEntry,
    pub child_session_ref: SessionRef,
}

/// Input for the isolated, read-only final synthesis transcript.
#[derive(Debug, Clone)]
pub struct TaskSynthesisSessionRunRequest {
    pub task: SequentialTaskRequest,
    pub attempt_id: TaskParticipantAttemptId,
    pub child_session_ref: SessionRef,
    pub plan_version: u32,
    pub child_input: AgentRunInput,
    pub options: AgentRunOptions,
}

/// Exact synthesis result returned to the parent single-writer commit boundary.
#[derive(Debug, Clone)]
pub struct TaskSynthesisSessionRunOutput {
    pub attempt_id: TaskParticipantAttemptId,
    pub final_text: String,
    pub outcome: AgentRunOutcome,
    pub child_session_ref: SessionRef,
    pub final_answer_ref: AgentFinalAnswerRef,
    pub artifact_refs: Vec<AgentArtifactRef>,
}

/// Structured output contract returned by a `ChangesetOnly` child writer.
#[derive(Debug, Clone)]
pub struct TaskChildChangeSetProposal {
    pub change_set: ChangeSet,
    pub artifact_ref: String,
    pub artifact: TaskChildChangeSetArtifact,
}

/// Reviewable artifact material emitted by a `ChangesetOnly` child writer.
#[derive(Debug, Clone)]
pub struct TaskChildChangeSetArtifact {
    pub media_type: String,
    pub content: String,
    pub content_sha256: String,
}

#[derive(Clone)]
pub(super) struct StepRunOutput {
    pub(super) final_text: String,
    pub(super) outcome: AgentRunOutcome,
    pub(super) final_answer_ref: Option<AgentFinalAnswerRef>,
    pub(super) artifact_refs: Vec<AgentArtifactRef>,
    pub(super) changeset_proposal: Option<TaskChildChangeSetProposal>,
    pub(super) changeset_only_after_snapshot_id: Option<String>,
}
