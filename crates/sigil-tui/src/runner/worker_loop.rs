use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use sigil_kernel::{
    Agent, AgentDelegationRequirement, AgentInvocationMode, AgentProfileId,
    AgentResultContinuationEntry, AgentResultContinuationStatus, AgentRole, AgentRunInput,
    AgentRunOptions, AgentRunResult, AgentThreadId, AgentThreadStatus,
    AgentThreadStatusChangedEntry, CheckDiscoverySource, CheckPromotion, CheckSpec,
    CheckSpecRecordedEntry, CompletionCriteria, ControlEntry, ConversationInputEditedEntry,
    ConversationInputKind, ConversationInputQueueControlAction, ConversationInputQueueControlEntry,
    ConversationInputQueueId, ConversationInputQueuedEntry, ConversationInputReorderedEntry,
    ConversationInputStatus, ConversationInputStatusEntry, ConversationInputTarget,
    ConversationQueueProjection, DEFAULT_TASK_VERIFICATION_SCOPE_HASH, DiscoveredCheck,
    EventHandler, EvidenceScope, ExecutionMutationProfile, JsonlSessionStore, ModelMessage,
    MutationArtifactLifecycleRecorded, MutationArtifactLifecycleStatus,
    MutationArtifactRetentionReport, MutationEventRecorder, PlanApprovalExpiry,
    PlanApprovalPermission, PlanApprovalScope, PlanApprovedEntry, PlanDecision, PlanDecisionActor,
    PlanDecisionRecordedEntry, PlanDraftCreatedEntry, PlanId, PlanPermissionGrantedEntry,
    PlanSourceRef, PlanTaskStartMode, ProviderCapabilities, ReasoningEffort, RootConfig, RunEvent,
    SandboxProfileRequirement, SequentialTaskOrchestrator, SequentialTaskRequest, Session,
    SessionLogEntry, SessionRef, SkillDescriptor, SkillRunMode, TaskChildSessionEntry,
    TaskChildSessionStatus, TaskCreatedFromPlanEntry, TaskId, TaskRouteId, TaskRouteStatus,
    TaskRunEntry, TaskRunProjection, TaskRunStatus, TaskStepEntry, TaskStepId, TaskStepSpec,
    TaskStepStatus, TaskSubagentElicitationRouteEntry, TerminalTaskEntry, TerminalTaskId,
    ToolApproval, ToolCall, ToolContext, ToolErrorKind, ToolExecutionEntry, ToolExecutionStatus,
    ToolRegistry, ToolResult, ToolResultMeta, ToolResultStatus, ToolSubject, ToolSubjectAudit,
    VerificationPolicy, VerificationPolicyChangedEntry, WorkspaceTrust,
    WorkspaceTrustDecisionEntry, WorkspaceTrustRequirement, build_workspace_snapshot,
    default_user_config_dir, discover_candidate_checks_with_user_config, plan_draft_created_entry,
    plan_task_input_from_draft, plan_text_hash, plan_workspace_paths, saturating_elapsed,
    stable_event_uuid, stable_workspace_id,
};

use sigil_runtime::{
    ProviderStatusTaskManager, ProviderStatusTaskResult, append_session_control_entries,
    current_unix_time_ms, effective_compaction_config,
};

use super::{
    approval_bridge::{ApprovalSignal, ChannelApprovalHandler},
    diagnostics::{changed_source_files, check_changed_files_diagnostics, diagnostics_tool_event},
    elicitation_bridge::{ChannelMcpElicitationHandler, McpElicitationAuditBuffer},
    event_bridge::ChannelEventHandler,
    mcp_event_bridge::{ChannelMcpRuntimeEventHandler, McpRuntimeEvent},
    protocol::{
        CompactionTrigger, McpActivationStatus, QueueMoveDirection, WorkerApprovalCommand,
        WorkerCommand, WorkerMessage,
    },
    session_flow::{auto_compact_session, load_session, session_compacted_message},
};

mod active_run;
mod agent_runtime;
mod mcp_refresh;
mod provider_status;
mod queue_driver;
mod scheduler;
mod task_runtime;
mod terminal_refresh;

pub(in crate::runner) use active_run::{
    ActiveRun, RunTaskPayload, RunTaskResult, cancel_active_run,
};
pub(in crate::runner) use agent_runtime::{
    WorkerAgentEventSink, agent_result_continuation_new_thread_ids, close_agent_thread,
    collect_finished_background_agent_runs, extend_agent_thread_ids_unique,
    manual_agent_invocation_result, manual_agent_parent_summary, message_agent_thread,
    start_agent_result_continuation_run, start_queued_conversation_run,
};
pub(in crate::runner) use mcp_refresh::WorkerLoopMcpHandlers;
pub(in crate::runner) use mcp_refresh::refresh_pending_mcp_servers;
pub(in crate::runner) use provider_status::drain_provider_status_results;
pub(in crate::runner) use queue_driver::{
    append_agent_result_continuation_status_and_notify,
    append_agent_result_continuation_status_entries, append_queue_failure_and_pause_and_notify,
    append_queue_status_and_notify, cancel_queued_conversation_input,
    edit_queued_conversation_input, mark_next_conversation_queue_item_dispatching,
    mark_stale_dispatching_conversation_queue_items, move_queued_conversation_input,
    promote_queued_conversation_input, queue_conversation_input, send_conversation_queue_update,
    set_conversation_queue_paused,
};
pub(in crate::runner) use scheduler::run_worker_loop;
pub(in crate::runner) use task_runtime::{
    CreateTaskFromPlanRequest, PlanApprovalRequest, SkillChildRunSpawn, TaskContinueSpawn,
    TaskRunSpawn, VerificationCheckPromotionKind, VerificationCheckPromotionOutcome,
    append_plan_draft, approve_plan, clean_mutation_artifacts, create_task_from_plan,
    delete_mutation_artifact, ensure_session_workspace_trust,
    format_mutation_artifact_cleanup_report, format_mutation_artifact_delete_report,
    load_worker_skill, next_task_id, plan_mode_transient_context,
    promote_workspace_verification_check, resolve_continue_task, session_ref_for_log_path,
    session_workspace_is_trusted, skill_child_session_objective, skill_invocation_prompt,
    spawn_skill_child_run, spawn_task_continue, spawn_task_run,
};
pub(in crate::runner) use terminal_refresh::{
    cancel_terminal_task, refresh_terminal_task_statuses,
};

#[cfg(test)]
pub(in crate::runner) use agent_runtime::chat_agent_run_input_with_repo_context;
#[cfg(test)]
pub(in crate::runner) use agent_runtime::queued_background_ready_transient_context;
pub(in crate::runner) use agent_runtime::{
    agent_delegation_requirement_for_prompt, append_mcp_elicitation_audits,
    partition_agent_result_continuations, pending_agent_result_continuations_from_session,
};
pub(in crate::runner) use task_runtime::append_cancelled_task_state;
#[cfg(test)]
pub(in crate::runner) use task_runtime::{
    materialize_task_verification_config, skill_child_agent_role,
};

const TERMINAL_TASK_REFRESH_INTERVAL: Duration = Duration::from_millis(500);
const MCP_REFRESH_RETRY_INTERVAL: Duration = Duration::from_millis(250);
