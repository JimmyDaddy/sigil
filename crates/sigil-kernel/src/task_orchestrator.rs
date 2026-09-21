use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
    sync::Arc,
};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    AgentRunInput, AgentRunOptions, AgentRunOutcome, AgentRunPurpose, ApprovalHandler, ChangeSet,
    CheckSpecId, CheckpointRestored, DEFAULT_TASK_VERIFICATION_SCOPE_HASH, DurableEventType,
    EventHandler, EvidenceScope, ExecutionMutationProfile, ModelMessage, MutationCommitted,
    MutationPrepared, MutationReconciled, MutationResolution, ReadinessEvaluatedEntry,
    ReadinessInput, RequiredAction, RunStatus, Session, SessionLogEntry, SessionStreamRecord,
    StoredEvent, TaskDirectExecutionContext, ToolAccess, ToolCategory, ToolExecutionStatus,
    ToolRegistry, ToolRegistryScope, ToolResultMeta, ToolSpec, TrustedCheckSpec,
    VerificationAutoRunPolicy, VerificationCheckRunEntry, VerificationCheckRunRequest,
    VerificationCheckRunStatus, VerificationPolicy, VerificationReceipt, VerificationRecordedEntry,
    VerificationScope, WorkspaceKnowledge, WorkspaceMutationDetected, WorkspaceMutationEvidence,
    WorkspaceSnapshotId, WorkspaceTrust, WriteIsolationMode, build_workspace_snapshot,
    build_workspace_snapshot_for_event, evaluate_readiness,
    session::ControlEntry,
    stable_workspace_id,
    task::{
        SessionRef, TaskExecutionAttemptStatus, TaskId, TaskRunEntry, TaskRunStatus, TaskStepId,
        bounded_task_participant_summary,
    },
    verification::PolicyHash,
    verification::{
        run_verification_check_with_evidence, verification_failure_locator_from_records,
        verification_receipt_link_from_records,
    },
    verification_check_run_id,
};
mod changeset_only;
mod child_session;
mod evidence;
mod prompts;
mod readiness;
mod runner;
mod shared;
mod types;

pub use changeset_only::{
    changeset_only_child_contract_prompt, changeset_only_child_tool_registry,
    changeset_only_child_tool_scope, decode_changeset_only_child_output,
};
pub use child_session::TaskChildSessionRunner;
pub use runner::DirectTaskRuntime;
pub use types::{
    DirectTaskRequest, DirectTaskRunOutput, TaskChildChangeSetArtifact, TaskChildChangeSetProposal,
    TaskDirectExecutionSessionRunOutput, TaskDirectExecutionSessionRunRequest,
    TaskVerificationRerunOutput, TaskVerificationRerunRequest,
};

use evidence::durable_workspace_mutation_evidence;
pub use prompts::task_direct_execution_system_prompt_contract_material;
use readiness::append_task_readiness;
pub use readiness::rerun_task_verification_check;
use shared::{
    append_task_control, append_task_control_with_event, append_task_controls, append_task_run,
};
