use super::*;
use serde::{Deserialize, Serialize};

/// Runtime-neutral request for executing a complete Task objective without a TaskPlan.
#[derive(Debug, Clone)]
pub struct TaskDirectExecutionSessionRunRequest {
    pub task: DirectTaskRequest,
    pub admission: crate::TaskDirectExecutionAdmittedV1,
    pub attempt: crate::TaskDirectExecutionAttemptV1,
    pub input: AgentRunInput,
    pub options: AgentRunOptions,
}

/// Bounded terminal output of one direct Task execution agent run.
#[derive(Debug, Clone)]
pub struct TaskDirectExecutionSessionRunOutput {
    pub attempt_id: String,
    pub final_text: String,
    pub final_message_id: Option<String>,
    pub outcome: AgentRunOutcome,
    pub disposition: crate::AgentRunDisposition,
}

/// Request for one direct Task run owned by the root model.
#[derive(Debug, Clone)]
pub struct DirectTaskRequest {
    pub task_id: TaskId,
    pub parent_session_ref: SessionRef,
    pub objective: String,
}

/// Durable terminal result of one direct Task run.
#[derive(Debug, Clone)]
pub struct DirectTaskRunOutput {
    pub task_id: TaskId,
    pub status: TaskRunStatus,
}

/// Exact task/check/policy binding and advisory workspace observation for a verification rerun.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskVerificationRerunRequest {
    pub request_id: String,
    pub task_id: TaskId,
    pub plan_version: u32,
    pub step_id: TaskStepId,
    pub check_spec_id: CheckSpecId,
    pub check_spec_hash: String,
    pub policy_hash: PolicyHash,
    pub workspace_snapshot_id: Option<WorkspaceSnapshotId>,
}

impl TaskVerificationRerunRequest {
    #[must_use]
    pub fn new(
        task_id: TaskId,
        plan_version: u32,
        step_id: TaskStepId,
        check_spec_id: CheckSpecId,
        check_spec_hash: String,
        policy_hash: PolicyHash,
        workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    ) -> Self {
        let mut request = Self {
            request_id: String::new(),
            task_id,
            plan_version,
            step_id,
            check_spec_id,
            check_spec_hash,
            policy_hash,
            workspace_snapshot_id,
        };
        request.request_id = request.expected_request_id();
        request
    }

    /// Deterministic identity of the exact rendered action binding.
    #[must_use]
    pub fn expected_request_id(&self) -> String {
        let seed = serde_json::json!({
            "task_id": self.task_id,
            "plan_version": self.plan_version,
            "step_id": self.step_id,
            "check_spec_id": self.check_spec_id,
            "check_spec_hash": self.check_spec_hash,
            "policy_hash": self.policy_hash,
            "workspace_snapshot_id": self.workspace_snapshot_id,
        })
        .to_string();
        format!("verification-rerun-{}", crate::sha256_hex(seed.as_bytes()))
    }

    #[must_use]
    pub fn has_exact_identity(&self) -> bool {
        self.request_id == self.expected_request_id()
    }
}

/// Durable terminal records produced by one exact task verification rerun.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskVerificationRerunOutput {
    pub check_run: VerificationCheckRunEntry,
    pub verification: VerificationRecordedEntry,
}

/// Structured review proposal returned by an isolated child writer.
#[derive(Debug, Clone)]
pub struct TaskChildChangeSetProposal {
    pub change_set: ChangeSet,
    pub artifact_ref: String,
    pub artifact: TaskChildChangeSetArtifact,
    pub source_isolation: WriteIsolationMode,
    pub child_snapshot_id: Option<WorkspaceSnapshotId>,
    pub integration_facts: crate::IntegrationProposalFacts,
}

/// Reviewable artifact material emitted by an isolated child writer.
#[derive(Debug, Clone)]
pub struct TaskChildChangeSetArtifact {
    pub media_type: String,
    pub content: String,
    pub content_sha256: String,
}
