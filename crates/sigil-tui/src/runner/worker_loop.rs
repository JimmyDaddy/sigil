use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use sigil_kernel::{
    Agent, AgentInvocationMode, AgentProfileId, AgentResultContinuationEntry,
    AgentResultContinuationStatus, AgentRole, AgentRunDisposition, AgentRunInput, AgentRunOptions,
    AgentRunResult, AgentThreadId, AgentThreadStatus, AgentThreadStatusChangedEntry,
    ApprovalHandler, CheckPromotion, CheckSpec, CheckSpecRecordedEntry, ControlEntry,
    ControlledCheckpointRestoreOutput, ControlledCheckpointRestorePreview,
    ControlledCheckpointRestoreRequest, ConversationForkOutput, ConversationForkRequest,
    ConversationInputEditedEntry, ConversationInputKind, ConversationInputQueueControlAction,
    ConversationInputQueueControlEntry, ConversationInputQueueId, ConversationInputQueuedEntry,
    ConversationInputReorderedEntry, ConversationInputStatus, ConversationInputStatusEntry,
    ConversationInputTarget, ConversationQueueProjection, DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
    EvidenceScope, ExecutionMutationProfile, ImageAttachmentResolver, JsonlSessionStore,
    MemoryConfig, ModelMessage, MutationArtifactLifecycleRecorded, MutationArtifactLifecycleStatus,
    MutationArtifactRetentionReport, MutationEventRecorder, PlanApprovalPermission,
    PlanDecisionRecordedEntry, PlanDraftCreatedEntry, PlanId, PlanSourceRef, PlanTaskStartMode,
    ProviderCapabilities, ReasoningEffort, RootConfig, RunCancellationFinalizedEntry,
    RunCancellationHandle, RunCancellationOwner, RunCancellationRecorder,
    RunCancellationRequestedEntry, RunCancellationTarget, RunCancellationTerminalOutcome, RunEvent,
    RunQuiescenceOutcome, RunTaskGuard, RuntimeContextCandidates, SecretString,
    SequentialTaskOrchestrator, SequentialTaskRequest, Session, SessionLogEntry, SessionRef,
    SkillDescriptor, SkillRunMode, StartDurableTaskAction, TaskChildSessionEntry,
    TaskChildSessionStatus, TaskGuidancePromotedEntry, TaskId, TaskPlanStatus, TaskRouteId,
    TaskRouteStatus, TaskRunProjection, TaskRunStatus, TaskStepId, TaskStepSpec,
    TaskSubagentElicitationRouteEntry, TerminalTaskEntry, TerminalTaskId, ToolApproval,
    ToolArtifactReadBudgetV1, ToolCall, ToolContext, ToolErrorKind, ToolExecutionEntry,
    ToolExecutionStatus, ToolRegistry, ToolResult, ToolResultMeta, ToolResultStatus, ToolSubject,
    ToolSubjectAudit, UserUrlCapabilityRegistrar, WorkspaceTrust, WorkspaceTrustDecisionEntry,
    build_workspace_snapshot, default_user_config_dir, discover_candidate_checks_with_user_config,
    plan_draft_created_entry, rerun_task_verification_check, saturating_elapsed, stable_event_uuid,
    stable_workspace_id,
};

use sigil_kernel::session::{
    ActiveProjectionFamily, ActiveProjectionFrontier, ActiveProjectionObserver,
    ActiveProjectionSubscription,
};
use sigil_runtime::{
    ConversationCoordinator, ConversationSourceTurn, ProviderStatusTaskManager,
    ProviderStatusTaskResult, append_session_control_entries,
    append_session_control_entries_and_track_detached, current_unix_time_ms,
};

use super::{
    approval_bridge::{ApprovalSignal, ChannelApprovalHandler},
    diagnostics::{changed_source_files, check_changed_files_diagnostics, diagnostics_tool_event},
    elicitation_bridge::{ChannelMcpElicitationHandler, McpElicitationAuditBuffer},
    event_bridge::ChannelEventHandler,
    mcp_event_bridge::{ChannelMcpRuntimeEventHandler, McpRuntimeEvent},
    protocol::{
        McpActivationStatus, McpOAuthUserAction, QueueMoveDirection, TerminalTaskControlIdentity,
        ToolArtifactDisplayReadFailure, V2CompactionApplySource, WORKER_COMMAND_PROTOCOL_VERSION,
        WorkerApprovalCommand, WorkerApprovalCommandReceipt, WorkerApprovalDecision,
        WorkerApprovalRouteState, WorkerCommand, WorkerMessage,
    },
    session_flow::{
        load_routed_session_with_runtime_attachments, load_session,
        load_session_with_runtime_attachments,
    },
    terminal_lifecycle_bridge::ChannelTerminalLifecycleRouter,
    worker_event::{
        WorkerActiveProjectionObserver, WorkerEvent, WorkerEventInbox, WorkerEventPayloadSender,
        WorkerReadiness, WorkerWakeCoalescer,
    },
};

mod active_run;
mod advancement;
mod agent_runtime;
mod artifact_gc_tasks;
mod checkpoint_runtime;
mod command_dispatch;
mod compaction_runtime;
mod compaction_tasks;
mod mcp_oauth_runtime;
mod mcp_refresh;
mod provider_status;
mod queue_driver;
mod scheduler;
mod session_lifecycle_runtime;
mod session_transition;
mod state;
mod task_runtime;
mod terminal_control;

pub(in crate::runner) use active_run::{
    ActiveRun, ActiveRunStopDisposition, RunTaskPayload, RunTaskResult,
    bind_task_run_cancellation_scope, cancel_active_run, prepare_run_cancellation,
    prepare_task_run_cancellation,
};
pub(in crate::runner) use advancement::{
    WorkerAdvancementContext, WorkerAdvancementControl, advance_worker_loop,
};
#[cfg(test)]
pub(in crate::runner) use advancement::{
    changed_task_completion_progress, changed_task_provider_route_diagnostics,
    pending_agent_continuations_from_active_projection, task_completion_progress_for_active_task,
};
#[cfg(test)]
pub(in crate::runner) use agent_runtime::agent_result_continuation_run_result;
pub(in crate::runner) use agent_runtime::{
    WorkerAgentEventSink, WorkerSupervisorEventSink, agent_result_continuation_new_thread_ids,
    cancel_agent_thread, close_agent_thread, collect_finished_background_agent_runs,
    extend_agent_thread_ids_unique, manual_agent_invocation_result, manual_agent_parent_summary,
    message_agent_thread, start_agent_result_continuation_run,
    start_portable_overflow_recovery_run, start_queued_conversation_run,
};
pub(in crate::runner) use artifact_gc_tasks::{ArtifactGcTaskManager, ArtifactGcTaskResult};
pub use artifact_gc_tasks::{ArtifactGcTaskMetricsSnapshot, artifact_gc_task_metrics};
pub(in crate::runner) use checkpoint_runtime::{
    execute_current_checkpoint_restore, fork_current_conversation,
    preview_current_checkpoint_restore,
};
pub(in crate::runner) use command_dispatch::{
    WorkerCommandContext, WorkerCommandDispatchControl, dispatch_worker_command,
};
#[cfg(test)]
pub(in crate::runner) use command_dispatch::{
    WorkerCommandDomain, classify_worker_command, read_tool_artifact_page_for_display,
    validate_task_pause_request,
};
pub(in crate::runner) use compaction_runtime::{
    IdleAutoCompactionPreflightDecision, IdleAutoCompactionPreparation,
    IdleAutoCompactionSchedulerEligibility, IdleAutoCompactionState, PendingLocalV2Compaction,
    PendingV2Compaction, QueuedConversationPreTurnAdmission,
    capture_stable_idle_compaction_snapshot, exact_context_window_rejection_source,
    has_failed_idle_automatic_scope, idle_auto_compaction_preflight, prepare_idle_auto_compaction,
    prepare_next_queued_conversation_pre_turn_admission, prepare_overflow_recovery_compaction,
    prepare_v2_compaction_review, prepare_v2_compaction_summary_review,
};
pub(in crate::runner) use compaction_tasks::{
    CompactionPreparationTaskManager, CompactionPreparationTaskResult, IdleV2CompactionPreparation,
    ManualV2CompactionPreparation, OverflowV2CompactionPreparation, PreTurnV2CompactionPreparation,
};
pub(in crate::runner) use mcp_oauth_runtime::{
    ActiveMcpOAuthFlow, McpOAuthTaskResult, advance_mcp_oauth_results, cancel_all_mcp_oauth_flows,
    dispatch_mcp_oauth_action,
};
pub(in crate::runner) use mcp_refresh::WorkerLoopMcpHandlers;
pub(in crate::runner) use mcp_refresh::refresh_pending_mcp_servers;
pub(in crate::runner) use provider_status::{
    drain_provider_status_results, provider_status_result_sender,
};
pub(in crate::runner) use queue_driver::{
    AdmittedQueuedConversationCandidate, DurableQueuePendingInputProvider,
    ExactConversationPromptStore, PreparedQueuedConversationCandidate,
    QueuedConversationPressureAdmission, QueuedConversationTerminalClassification,
    TaskGuidancePreparation, append_agent_result_continuation_status_and_notify,
    append_agent_result_continuation_status_entries, append_queue_failure_and_pause_and_notify,
    append_queue_status_and_notify, cancel_queued_conversation_input,
    classify_promoted_queued_conversation, commit_prepared_queued_conversation_candidate,
    edit_queued_conversation_input, mark_stale_dispatching_conversation_queue_items,
    move_queued_conversation_input,
    prepare_next_queued_conversation_pressure_admission_with_resolver,
    prepare_next_task_guidance_candidate, promote_queued_conversation_input,
    queue_conversation_input_and_track_detached, send_conversation_queue_update,
    set_conversation_queue_paused,
};
#[cfg(test)]
#[allow(unused_imports)]
pub(in crate::runner) use queue_driver::{
    QueuedConversationCandidatePreparation, prepare_next_queued_conversation_candidate,
    prepare_next_queued_conversation_pressure_admission, queue_conversation_input,
};
pub(in crate::runner) use scheduler::{
    WorkerLoopSessionAttachment, WorkerLoopTerminalRuntime, finish_idle_auto_compaction,
    run_worker_loop,
};
pub use scheduler::{WorkerReactorMetricsSnapshot, worker_reactor_metrics};
pub(in crate::runner) use session_lifecycle_runtime::{
    apply_local_session_delete, apply_session_retention, export_local_session, fork_local_session,
    inspect_local_session, local_session_lifecycle_service,
    local_session_lifecycle_service_for_source, local_session_lifecycle_service_with_scratch,
    preview_local_session_delete, preview_session_retention, set_local_session_pin,
};
pub(in crate::runner) use session_transition::{
    SessionTransitionKind, ensure_session_transition_allowed, session_transition_recovery_message,
    transition_session, transition_session_with_attachment,
    transition_session_with_attachment_recovery,
};
pub(in crate::runner) use state::{WorkerLoopState, register_worker_active_projection_observer};
pub(in crate::runner) use task_runtime::{
    AdmittedTaskRunOrchestration, CreateTaskFromPlanRequest, RejectPlanRequest,
    RoutedTaskContinuationOrchestration, SkillChildRunSpawn, TaskContinueSpawn,
    TaskPlannerInputSpawn, TaskRunSpawn, VerificationCheckPromotionKind,
    VerificationCheckPromotionOutcome, append_plan_draft, clean_mutation_artifacts,
    continue_routed_task_to_root_terminal, create_task_from_plan, delete_mutation_artifact,
    ensure_session_workspace_trust, format_mutation_artifact_cleanup_report,
    format_mutation_artifact_delete_report, load_worker_skill, next_task_id,
    plan_mode_transient_context, promote_workspace_verification_check, reject_plan,
    resolve_continue_task, revise_plan, run_admitted_task_to_root_terminal,
    session_ref_for_log_path, session_workspace_is_trusted, skill_child_session_objective,
    skill_invocation_prompt, spawn_skill_child_run, spawn_task_continue, spawn_task_planner_input,
    spawn_task_run,
};
#[cfg(test)]
pub(in crate::runner) use terminal_control::durable_terminal_tool_result_metadata;
pub(in crate::runner) use terminal_control::{
    cancel_terminal_task, terminal_start_execution_profile_for_task,
};

#[cfg(test)]
pub(in crate::runner) use agent_runtime::chat_agent_run_input_with_repo_context;
pub(in crate::runner) use agent_runtime::{
    append_mcp_elicitation_audits, partition_agent_result_continuations,
    pending_agent_result_continuations_from_session, queued_background_ready_transient_context,
};
pub(in crate::runner) use sigil_runtime::agent_supervisor::task_role_runtime::{
    RuntimeTaskRoleProviderBuilder, TaskRoleProviderBuilder, TaskRoleRuntime,
};
pub(in crate::runner) use task_runtime::{
    append_cancelled_task_state, append_interrupted_task_state, append_paused_task_state,
};
#[cfg(test)]
pub(in crate::runner) use task_runtime::{
    configured_max_parallel_changeset_steps, configured_max_parallel_read_steps,
    configured_provider_route_concurrency_limit, materialize_task_verification_config,
    plan_handoff_workspace_snapshot_id, skill_child_agent_role,
};

const MCP_REFRESH_RETRY_INTERVAL: Duration = Duration::from_millis(250);
