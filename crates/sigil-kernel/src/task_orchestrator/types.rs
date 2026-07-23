use super::*;
use serde::{Deserialize, Serialize};

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
