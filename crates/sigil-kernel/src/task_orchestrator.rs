use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    Agent, AgentRunInput, AgentRunOptions, AgentRunOutcome, AgentRunTerminalReason,
    ApprovalHandler, ChangeSet, CheckPromotion, CheckpointRestored, CompletionCriteria,
    DEFAULT_TASK_VERIFICATION_SCOPE_HASH, DurableEventType, EventHandler, EvidenceScope,
    ExecutionBackend, ExecutionMutationProfile, FileType, JsonlSessionStore, MergeReviewId,
    MergeReviewRequested, ModelMessage, MutationCommitted, MutationPrepared, MutationReconciled,
    MutationResolution, MutationSubject, Provider, ReadinessEvaluatedEntry, ReadinessInput,
    RequiredAction, RunEvent, RunStatus, Session, SessionLogEntry, SessionStreamRecord,
    StoredEvent, ToolAccess, ToolCategory, ToolErrorKind, ToolExecutionStatus, ToolRegistry,
    ToolRegistryScope, ToolResultMeta, ToolSpec, VerificationAutoRunPolicy,
    VerificationCheckRunEntry, VerificationCheckRunRequest, VerificationCheckRunStatus,
    VerificationPolicy, VerificationReceipt, VerificationScope, VerificationVerdict,
    VisibleCompletionState, WorkspaceKnowledge, WorkspaceMutationDetected,
    WorkspaceMutationEvidence, WorkspaceTrust, WriteIsolationMode, WriteLeaseAcquired,
    WriteLeaseId, WriteLeaseReleaseStatus, WriteLeaseReleased, WriteLeaseScope,
    build_workspace_snapshot_for_event, evaluate_readiness, run_verification_check,
    session::ControlEntry,
    stable_event_uuid, stable_workspace_id,
    task::{
        AgentRole, SessionRef, TaskId, TaskIsolationMode, TaskPlanEntry, TaskPlanStatus,
        TaskPlanUpdateContext, TaskReadyDeferredReason, TaskReadyQueueOptions, TaskRunEntry,
        TaskRunProjection, TaskRunStatus, TaskStepEntry, TaskStepId, TaskStepMode, TaskStepSpec,
        TaskStepStatus,
    },
    verification_check_run_id,
};
#[cfg(test)]
use crate::{
    ToolApproval, ToolCall,
    task::{
        TaskChildSessionEntry, TaskChildSessionStatus, TaskRouteId, TaskRouteStatus,
        TaskSubagentApprovalRouteEntry, child_session_ref,
    },
};

type BoxedAgent = Agent<Box<dyn Provider>>;

mod changeset_only;
mod child_session;
mod evidence;
mod prompts;
mod readiness;
mod runner;
mod scheduler;
mod shared;
mod types;
mod write_lease;

pub use changeset_only::{
    changeset_only_child_contract_prompt, changeset_only_child_tool_registry,
    changeset_only_child_tool_scope, decode_changeset_only_child_output,
    validate_changeset_only_parent_snapshot_unchanged_for_task,
};
pub use child_session::TaskChildSessionRunner;
pub use runner::SequentialTaskOrchestrator;
pub use types::{
    SequentialTaskRequest, SequentialTaskRunOutput, SequentialTaskStepOutput,
    TaskChildChangeSetArtifact, TaskChildChangeSetProposal, TaskChildSessionRunOutput,
    TaskChildSessionRunRequest,
};

use changeset_only::{
    capture_changeset_only_parent_snapshot_id, record_changeset_only_child_output,
    with_changeset_only_child_contract,
};
use evidence::{
    changed_files_mutation_evidence, durable_mutation_replay_failed_evidence,
    durable_workspace_mutation_evidence,
};
use prompts::{
    executor_step_prompt, normalize_task_guidance, planner_prompt, subagent_step_prompt,
    task_continue_reason,
};
use readiness::{
    append_task_readiness, run_task_step_verification_checks, task_step_auto_run_policy,
    task_step_failure_readiness_nonblocking, task_step_readiness_nonblocking,
    task_step_verification_scope_hash,
};
#[cfg(test)]
use readiness::{
    latest_relevant_successful_verification_sequence, relevant_verification_receipts,
    task_step_default_policy, task_step_readiness,
};
#[cfg(test)]
#[path = "tests/task_orchestrator_child_session_test_support.rs"]
mod task_orchestrator_child_session_test_support;
use scheduler::{
    append_cancelled_dependent_steps, cancels_dependent_steps, latest_executable_plan,
    run_status_from_step_status, runnable_steps_for_continue, step_reason_from_output,
    step_status_after_readiness, step_status_from_outcome, step_terminal_reason,
    task_status_from_step_status,
};
use shared::{append_task_control, append_task_run, append_task_step};
#[cfg(test)]
use shared::{hash_text, route_id_for_call};
#[cfg(test)]
use task_orchestrator_child_session_test_support::{
    TestAgentTaskChildSessionRunner, child_status_from_output,
};
use types::StepRunOutput;
use write_lease::{
    acquire_task_write_lease, release_task_write_lease, write_lease_release_status_from_step_status,
};

#[cfg(test)]
#[path = "tests/task_orchestrator_tests.rs"]
mod tests;
