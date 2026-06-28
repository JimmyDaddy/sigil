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
    PlanApprovalPermission, PlanApprovalScope, PlanApprovedEntry, ProviderCapabilities,
    ReasoningEffort, RootConfig, RunEvent, SandboxProfileRequirement, SequentialTaskOrchestrator,
    SequentialTaskRequest, Session, SessionLogEntry, SessionRef, SkillDescriptor, SkillRunMode,
    TaskChildSessionEntry, TaskChildSessionStatus, TaskId, TaskRouteId, TaskRouteStatus,
    TaskRunEntry, TaskRunProjection, TaskRunStatus, TaskStepEntry, TaskStepId, TaskStepSpec,
    TaskStepStatus, TaskSubagentElicitationRouteEntry, TerminalTaskEntry, TerminalTaskId,
    ToolApproval, ToolCall, ToolContext, ToolErrorKind, ToolExecutionEntry, ToolExecutionStatus,
    ToolRegistry, ToolResult, ToolResultMeta, ToolResultStatus, ToolSubject, ToolSubjectAudit,
    VerificationPolicy, VerificationPolicyChangedEntry, WorkspaceTrust,
    WorkspaceTrustDecisionEntry, WorkspaceTrustRequirement, default_user_config_dir,
    discover_candidate_checks_with_user_config, plan_text_hash, plan_workspace_paths,
    saturating_elapsed, stable_event_uuid, stable_workspace_id,
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
        CompactionTrigger, McpActivationStatus, QueueMoveDirection, WorkerCommand, WorkerMessage,
    },
    session_flow::{auto_compact_session, load_session, session_compacted_message},
};

const TERMINAL_TASK_REFRESH_INTERVAL: Duration = Duration::from_millis(500);
const MCP_REFRESH_RETRY_INTERVAL: Duration = Duration::from_millis(250);

pub(super) fn run_worker_loop<P>(
    runtime: tokio::runtime::Runtime,
    mut agent: Arc<Agent<P>>,
    root_config: RootConfig,
    provider_capabilities: ProviderCapabilities,
    workspace_root: PathBuf,
    session_log_path: PathBuf,
    options: AgentRunOptions,
    command_rx: mpsc::Receiver<WorkerCommand>,
    message_tx: mpsc::Sender<WorkerMessage>,
    mcp_handlers: WorkerLoopMcpHandlers,
) where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerLoopMcpHandlers {
        elicitation_handler,
        event_handler: mcp_event_handler,
        event_rx: mcp_event_rx,
    } = mcp_handlers;
    let mut current_session_log_path = session_log_path;
    let mut current_session = match load_session(
        &root_config.agent.provider,
        &root_config.agent.model,
        &current_session_log_path,
    ) {
        Ok(mut session) => {
            mark_stale_dispatching_conversation_queue_items(&mut session, &message_tx);
            Some(session)
        }
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
            return;
        }
    };

    let (task_result_tx, task_result_rx) = mpsc::channel::<RunTaskResult>();
    let (provider_status_tx, provider_status_rx) = mpsc::channel::<ProviderStatusTaskResult>();
    let mut active_run: Option<ActiveRun> = None;
    let mut provider_status_tasks = ProviderStatusTaskManager::new();
    let mut next_run_id = 1_u64;
    let mut discarded_run_ids = BTreeSet::new();
    let mut pending_mcp_refreshes = BTreeSet::new();
    let mut next_mcp_refresh_retry_at = Instant::now();
    let mut pending_agent_result_continuations =
        pending_agent_result_continuations_from_session(current_session.as_ref());
    let mut next_terminal_task_refresh_at = Instant::now();
    let session_entries = current_session
        .as_ref()
        .map(Session::entries)
        .unwrap_or(&[]);
    let agent_supervisor =
        match sigil_runtime::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &root_config,
            &workspace_root,
            session_entries,
        ) {
            Ok(registry) => sigil_runtime::AgentSupervisor::new(
                registry,
                sigil_runtime::AgentBudgetPolicy::from_root_config(&root_config),
                provider_capabilities.clone(),
            ),
            Err(error) => {
                let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                return;
            }
        };
    let background_agent_runs =
        sigil_runtime::AgentToolBackgroundRuns::with_event_sink(Arc::new(WorkerAgentEventSink {
            sender: message_tx.clone(),
        }));

    loop {
        while let Ok(event) = mcp_event_rx.try_recv() {
            match event {
                McpRuntimeEvent::Progress(notification) => {
                    let _ = message_tx.send(WorkerMessage::McpProgress { notification });
                }
                McpRuntimeEvent::ListChanged(notification) => {
                    pending_mcp_refreshes.insert(notification.server_name.clone());
                    let _ = message_tx.send(WorkerMessage::McpListChanged { notification });
                }
            }
        }

        if active_run.is_none()
            && !pending_mcp_refreshes.is_empty()
            && Instant::now() >= next_mcp_refresh_retry_at
        {
            let shared_registry_blocked = refresh_pending_mcp_servers(
                &runtime,
                &mut agent,
                &root_config,
                &provider_capabilities,
                &options,
                &message_tx,
                Arc::clone(&elicitation_handler),
                Arc::clone(&mcp_event_handler),
                current_session
                    .as_ref()
                    .and_then(Session::mutation_event_recorder),
                &mut pending_mcp_refreshes,
            );
            next_mcp_refresh_retry_at = if shared_registry_blocked {
                Instant::now() + MCP_REFRESH_RETRY_INTERVAL
            } else {
                Instant::now()
            };
        }

        while let Ok(status_result) = provider_status_rx.try_recv() {
            match status_result {
                ProviderStatusTaskResult::Balance {
                    request_id,
                    snapshot,
                } => {
                    if provider_status_tasks.accept_balance_result(request_id) {
                        let _ = message_tx.send(WorkerMessage::ProviderBalanceRefreshed {
                            request_id,
                            snapshot,
                        });
                    }
                }
                ProviderStatusTaskResult::Models {
                    request_id,
                    base_url,
                    result,
                } => {
                    if provider_status_tasks.accept_models_result(request_id) {
                        let _ = message_tx.send(WorkerMessage::ProviderModelsRefreshed {
                            request_id,
                            base_url,
                            result,
                        });
                    }
                }
            }
        }

        while let Ok(task_result) = task_result_rx.try_recv() {
            if discarded_run_ids.remove(&task_result.run_id) {
                continue;
            }
            elicitation_handler.set_audit_buffer(None);
            active_run = None;
            current_session = match load_session(
                task_result.session.provider_name(),
                task_result.session.model_name(),
                &current_session_log_path,
            ) {
                Ok(session) => Some(session),
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                        "session reload skipped after run: {error:#}"
                    )));
                    Some(task_result.session)
                }
            };
            let auto_compaction = match current_session.as_mut() {
                Some(session) => {
                    let effective_config = effective_compaction_config(
                        session.provider_name(),
                        session.model_name(),
                        &options.compaction_config,
                    );
                    match auto_compact_session(session, &effective_config) {
                        Ok(record) => record,
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::Notice(format!(
                                "automatic compaction skipped: {error}",
                            )));
                            None
                        }
                    }
                }
                None => None,
            };
            match task_result.payload {
                RunTaskPayload::Chat {
                    result: Ok(run_result),
                    plan_mode,
                    queue_id,
                    agent_result_continuation_thread_ids,
                } => {
                    if let Some(queue_id) = queue_id {
                        append_queue_status_and_notify(
                            &mut current_session,
                            &message_tx,
                            queue_id,
                            ConversationInputStatus::Delivered,
                            None,
                        );
                    }
                    if !agent_result_continuation_thread_ids.is_empty() {
                        append_agent_result_continuation_status_and_notify(
                            &mut current_session,
                            &message_tx,
                            &agent_result_continuation_thread_ids,
                            AgentResultContinuationStatus::Completed,
                            Some("parent continuation completed"),
                        );
                    }
                    let entries = current_session
                        .as_ref()
                        .map(|session| session.entries().to_vec())
                        .unwrap_or_default();
                    let message = if plan_mode {
                        WorkerMessage::PlanRunFinished {
                            result: run_result,
                            entries,
                        }
                    } else {
                        WorkerMessage::RunFinished {
                            result: run_result,
                            entries,
                        }
                    };
                    let _ = message_tx.send(message);
                }
                RunTaskPayload::Agent {
                    profile_id,
                    result: Ok(run_result),
                } => {
                    let entries = current_session
                        .as_ref()
                        .map(|session| session.entries().to_vec())
                        .unwrap_or_default();
                    let _ = message_tx.send(WorkerMessage::AgentRunFinished {
                        profile_id,
                        result: run_result,
                        entries,
                    });
                }
                RunTaskPayload::Chat {
                    result: Err(error),
                    queue_id,
                    agent_result_continuation_thread_ids,
                    ..
                } => {
                    if let Some(queue_id) = queue_id {
                        append_queue_failure_and_pause_and_notify(
                            &current_session_log_path,
                            &mut current_session,
                            &message_tx,
                            queue_id,
                            error.clone(),
                        );
                    }
                    if !agent_result_continuation_thread_ids.is_empty() {
                        append_agent_result_continuation_status_and_notify(
                            &mut current_session,
                            &message_tx,
                            &agent_result_continuation_thread_ids,
                            AgentResultContinuationStatus::Failed,
                            Some(error.as_str()),
                        );
                    }
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                }
                RunTaskPayload::Agent {
                    result: Err(error), ..
                } => {
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                }
                RunTaskPayload::Task {
                    task_id,
                    result: Ok(status),
                } => {
                    let entries = current_session
                        .as_ref()
                        .map(|session| session.entries().to_vec())
                        .unwrap_or_default();
                    let _ = message_tx.send(WorkerMessage::TaskRunFinished {
                        task_id,
                        status,
                        entries,
                    });
                }
                RunTaskPayload::Task {
                    task_id: _,
                    result: Err(error),
                } => {
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                }
            }
            if let (Some(session), Some(record)) = (current_session.as_ref(), auto_compaction) {
                let _ = message_tx.send(session_compacted_message(
                    &current_session_log_path,
                    session,
                    record,
                    CompactionTrigger::AutomaticHardThreshold,
                ));
            }
        }

        if active_run.is_none() {
            if Instant::now() >= next_terminal_task_refresh_at {
                next_terminal_task_refresh_at = Instant::now() + TERMINAL_TASK_REFRESH_INTERVAL;
                match refresh_terminal_task_statuses(
                    &runtime,
                    agent.tool_registry(),
                    &options,
                    &current_session_log_path,
                    &mut current_session,
                ) {
                    Ok(updates) => {
                        for (entry, entries) in updates {
                            let _ = message_tx
                                .send(WorkerMessage::TerminalTaskUpdated { entry, entries });
                        }
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                }
            }

            let completed_agent_threads = collect_finished_background_agent_runs(
                &runtime,
                &background_agent_runs,
                &agent_supervisor,
                &root_config,
                agent.tool_registry(),
                &mut current_session,
                &message_tx,
            );
            if !completed_agent_threads.is_empty() {
                let new_continuation_threads = agent_result_continuation_new_thread_ids(
                    current_session.as_ref(),
                    &completed_agent_threads,
                );
                if !new_continuation_threads.is_empty()
                    && let Err(error) = append_agent_result_continuation_status_entries(
                        &current_session_log_path,
                        &mut current_session,
                        &new_continuation_threads,
                        AgentResultContinuationStatus::Pending,
                        Some("child agent result ready"),
                    )
                {
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    continue;
                }
                let (blocking, non_blocking) = partition_agent_result_continuations(
                    current_session.as_ref(),
                    completed_agent_threads,
                );
                extend_agent_thread_ids_unique(
                    &mut pending_agent_result_continuations,
                    non_blocking,
                );
                let queued_input_ready = current_session
                    .as_ref()
                    .and_then(|session| session.conversation_queue_projection().next_dispatchable)
                    .is_some();
                let mut continuation_threads = blocking;
                if !queued_input_ready {
                    continuation_threads.append(&mut pending_agent_result_continuations);
                }
                if continuation_threads.is_empty() {
                    continue;
                }
                active_run = start_agent_result_continuation_run(
                    &runtime,
                    Arc::clone(&agent),
                    &agent_supervisor,
                    &root_config,
                    &current_session_log_path,
                    agent.tool_registry(),
                    &options,
                    &background_agent_runs,
                    &mut current_session,
                    &task_result_tx,
                    &message_tx,
                    Arc::clone(&elicitation_handler),
                    &mut next_run_id,
                    continuation_threads,
                );
                if active_run.is_some() {
                    continue;
                }
            }
        }

        if active_run.is_none() {
            let queued_input_ready = current_session
                .as_ref()
                .and_then(|session| session.conversation_queue_projection().next_dispatchable)
                .is_some();
            if !queued_input_ready && !pending_agent_result_continuations.is_empty() {
                let continuation_threads = std::mem::take(&mut pending_agent_result_continuations);
                active_run = start_agent_result_continuation_run(
                    &runtime,
                    Arc::clone(&agent),
                    &agent_supervisor,
                    &root_config,
                    &current_session_log_path,
                    agent.tool_registry(),
                    &options,
                    &background_agent_runs,
                    &mut current_session,
                    &task_result_tx,
                    &message_tx,
                    Arc::clone(&elicitation_handler),
                    &mut next_run_id,
                    continuation_threads,
                );
                if active_run.is_some() {
                    continue;
                }
            }
        }

        if active_run.is_none()
            && let Some(queued) =
                mark_next_conversation_queue_item_dispatching(&mut current_session, &message_tx)
        {
            active_run = start_queued_conversation_run(
                &runtime,
                Arc::clone(&agent),
                &agent_supervisor,
                &root_config,
                agent.tool_registry(),
                &options,
                &background_agent_runs,
                &mut current_session,
                &task_result_tx,
                &message_tx,
                Arc::clone(&elicitation_handler),
                &mut next_run_id,
                queued,
            );
            if active_run.is_some() {
                continue;
            }
        }

        match command_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(WorkerCommand::QueueConversationInput {
                prompt,
                kind,
                target,
                reasoning_effort,
            }) => match queue_conversation_input(
                &current_session_log_path,
                &mut current_session,
                prompt,
                kind,
                target,
                reasoning_effort,
            ) {
                Ok(entries) => {
                    send_conversation_queue_update(&message_tx, &entries);
                }
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                }
            },
            Ok(WorkerCommand::CancelQueuedConversationInput { queue_id }) => {
                match cancel_queued_conversation_input(
                    &current_session_log_path,
                    &mut current_session,
                    queue_id,
                ) {
                    Ok(entries) => send_conversation_queue_update(&message_tx, &entries),
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::EditQueuedConversationInput {
                queue_id,
                prompt,
                reasoning_effort,
            }) => match edit_queued_conversation_input(
                &current_session_log_path,
                &mut current_session,
                queue_id,
                prompt,
                reasoning_effort,
            ) {
                Ok(entries) => send_conversation_queue_update(&message_tx, &entries),
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                }
            },
            Ok(WorkerCommand::MoveQueuedConversationInput {
                queue_id,
                direction,
            }) => match move_queued_conversation_input(
                &current_session_log_path,
                &mut current_session,
                queue_id,
                direction,
            ) {
                Ok(entries) => send_conversation_queue_update(&message_tx, &entries),
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                }
            },
            Ok(WorkerCommand::PromoteQueuedConversationInput { queue_id }) => {
                match promote_queued_conversation_input(
                    &current_session_log_path,
                    &mut current_session,
                    queue_id,
                ) {
                    Ok(entries) => send_conversation_queue_update(&message_tx, &entries),
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::SendQueuedConversationInputNow { queue_id }) => {
                match promote_queued_conversation_input(
                    &current_session_log_path,
                    &mut current_session,
                    queue_id,
                ) {
                    Ok(entries) => {
                        send_conversation_queue_update(&message_tx, &entries);
                        if let Some(run) = active_run.take() {
                            cancel_active_run(
                                run,
                                &root_config,
                                &current_session_log_path,
                                &mut current_session,
                                &message_tx,
                                &elicitation_handler,
                                &agent_supervisor,
                                &mut discarded_run_ids,
                                "run interrupted for queued input",
                            );
                        }
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::SetConversationQueuePaused { paused }) => {
                match set_conversation_queue_paused(
                    &current_session_log_path,
                    &mut current_session,
                    paused,
                ) {
                    Ok(entries) => send_conversation_queue_update(&message_tx, &entries),
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(
                command @ (WorkerCommand::SubmitPrompt { .. }
                | WorkerCommand::SubmitPlanPrompt { .. }),
            ) => {
                let (prompt, reasoning_effort, plan_mode) = match command {
                    WorkerCommand::SubmitPrompt {
                        prompt,
                        reasoning_effort,
                    } => (prompt, reasoning_effort, false),
                    WorkerCommand::SubmitPlanPrompt {
                        prompt,
                        reasoning_effort,
                    } => (prompt, reasoning_effort, true),
                    _ => unreachable!("matched submit prompt commands above"),
                };
                if active_run.is_some() {
                    let kind = if plan_mode {
                        ConversationInputKind::PlanPrompt
                    } else {
                        ConversationInputKind::Chat
                    };
                    match queue_conversation_input(
                        &current_session_log_path,
                        &mut current_session,
                        prompt,
                        kind,
                        ConversationInputTarget::MainThread,
                        reasoning_effort,
                    ) {
                        Ok(entries) => send_conversation_queue_update(&message_tx, &entries),
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        }
                    }
                    continue;
                }

                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let started = if plan_mode {
                    WorkerMessage::PlanRunStarted {
                        prompt: prompt.clone(),
                    }
                } else {
                    WorkerMessage::RunStarted {
                        prompt: prompt.clone(),
                    }
                };
                let _ = message_tx.send(started);

                let mut handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let agent = Arc::clone(&agent);
                let mut options = options.clone();
                options.reasoning_effort = Some(reasoning_effort);
                agent_supervisor.reset_turn_budget();
                let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
                    agent_supervisor.clone(),
                    root_config.clone(),
                    agent.tool_registry().clone(),
                )
                .with_background_runs(background_agent_runs.clone());
                let plan_tools = plan_mode.then(|| {
                    sigil_runtime::build_plan_prompt_tool_registry(
                        agent.tool_registry(),
                        &root_config,
                    )
                    .into_registry()
                });
                let task_result_tx = task_result_tx.clone();
                let run_id = next_run_id;
                next_run_id += 1;
                let delegation_requirement = agent_delegation_requirement_for_prompt(&prompt);

                let handle = runtime.spawn(async move {
                    let mut run_session = run_session;
                    let result = {
                        let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
                        let mut input = if plan_mode {
                            AgentRunInput::without_persisted_user_message(
                                plan_mode_transient_context(prompt),
                            )
                        } else {
                            AgentRunInput::user(prompt)
                        };
                        if let Some(requirement) = delegation_requirement {
                            input = input.with_agent_delegation_requirement(requirement);
                        }
                        if let Some(tools) = plan_tools {
                            agent
                                .run_with_approval_input_tool_registry_and_agent_delegate(
                                    &mut run_session,
                                    input,
                                    options,
                                    tools,
                                    &mut handler,
                                    &mut approval_handler,
                                    &mut agent_delegate,
                                )
                                .await
                        } else {
                            agent
                                .run_with_approval_input_and_agent_delegate(
                                    &mut run_session,
                                    input,
                                    options,
                                    &mut handler,
                                    &mut approval_handler,
                                    &mut agent_delegate,
                                )
                                .await
                        }
                        .map(|output| output.result)
                        .map_err(|error| format!("{error:#}"))
                    };
                    let result = match append_mcp_elicitation_audits(
                        &mut run_session,
                        &run_elicitation_audit_buffer,
                    ) {
                        Ok(()) => result,
                        Err(error) => Err(error),
                    };
                    let _ = task_result_tx.send(RunTaskResult {
                        run_id,
                        session: run_session,
                        payload: RunTaskPayload::Chat {
                            result,
                            plan_mode,
                            queue_id: None,
                            agent_result_continuation_thread_ids: Vec::new(),
                        },
                    });
                });

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                });
            }
            Ok(WorkerCommand::InvokeAgentProfile {
                profile_id,
                prompt,
                parent_prompt,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }

                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                let mut run_session = run_session;
                if let Err(error) =
                    run_session.append_user_message(ModelMessage::user(parent_prompt.clone()))
                {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                        "failed to persist agent invocation prompt: {error:#}"
                    )));
                    current_session = Some(run_session);
                    continue;
                }

                let _ = message_tx.send(WorkerMessage::AgentRunStarted {
                    profile_id: profile_id.clone(),
                    prompt: parent_prompt,
                });

                let mut handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                agent_supervisor.reset_turn_budget();
                let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
                    agent_supervisor.clone(),
                    root_config.clone(),
                    agent.tool_registry().clone(),
                )
                .with_background_runs(background_agent_runs.clone());
                let options = options.clone();
                let task_result_tx = task_result_tx.clone();
                let run_id = next_run_id;
                next_run_id += 1;

                let handle = runtime.spawn(async move {
                    let profile_id_for_summary = profile_id.clone();
                    let result = {
                        let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
                        match AgentProfileId::new(profile_id.clone()) {
                            Ok(profile_id_value) => agent_delegate
                                .invoke_agent_profile(
                                    &mut run_session,
                                    profile_id_value,
                                    prompt,
                                    &options,
                                    &mut handler,
                                    &mut approval_handler,
                                )
                                .await
                                .and_then(|invocation| {
                                    let run_result = manual_agent_invocation_result(&invocation);
                                    run_session.append_assistant_message(
                                        ModelMessage::assistant(
                                            Some(manual_agent_parent_summary(
                                                &profile_id_for_summary,
                                                &invocation,
                                            )),
                                            Vec::new(),
                                        ),
                                    )?;
                                    Ok(run_result)
                                })
                                .map_err(|error| format!("{error:#}")),
                            Err(error) => Err(format!("{error:#}")),
                        }
                    };
                    let result = match append_mcp_elicitation_audits(
                        &mut run_session,
                        &run_elicitation_audit_buffer,
                    ) {
                        Ok(()) => result,
                        Err(error) => Err(error),
                    };
                    let _ = task_result_tx.send(RunTaskResult {
                        run_id,
                        session: run_session,
                        payload: RunTaskPayload::Agent { profile_id, result },
                    });
                });

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                });
            }
            Ok(WorkerCommand::InvokeInlineSkill {
                skill_id,
                arguments,
                reasoning_effort,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }

                let run_id = next_run_id;
                let loaded =
                    match load_worker_skill(&root_config, &options, &skill_id, Some(run_id)) {
                        Ok(loaded) => loaded,
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                            continue;
                        }
                    };
                if loaded.descriptor.run_as != SkillRunMode::Inline {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                        "agent {skill_id} is configured for {} mode, not inline skill mode",
                        loaded.descriptor.run_as.as_str()
                    )));
                    continue;
                }
                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let prompt = skill_invocation_prompt(&skill_id, &arguments);
                let _ = message_tx.send(WorkerMessage::RunStarted {
                    prompt: prompt.clone(),
                });

                let mut handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let skill_registry = sigil_runtime::build_skill_tool_registry(
                    agent.tool_registry(),
                    &loaded.descriptor,
                )
                .into_registry();
                let agent = Arc::clone(&agent);
                let mut options = options.clone();
                options.reasoning_effort = Some(reasoning_effort);
                let task_result_tx = task_result_tx.clone();
                next_run_id += 1;

                let handle = runtime.spawn(async move {
                    let mut run_session = run_session;
                    let input = AgentRunInput::transient(prompt, vec![loaded.transient_context]);
                    let result =
                        match run_session.append_control(ControlEntry::SkillLoaded(loaded.entry)) {
                            Ok(()) => {
                                let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
                                agent
                                    .run_with_approval_input_and_tool_registry(
                                        &mut run_session,
                                        input,
                                        options,
                                        skill_registry,
                                        &mut handler,
                                        &mut approval_handler,
                                    )
                                    .await
                                    .map(|output| output.result)
                                    .map_err(|error| format!("{error:#}"))
                            }
                            Err(error) => Err(format!("{error:#}")),
                        };
                    let result = match append_mcp_elicitation_audits(
                        &mut run_session,
                        &run_elicitation_audit_buffer,
                    ) {
                        Ok(()) => result,
                        Err(error) => Err(error),
                    };
                    let _ = task_result_tx.send(RunTaskResult {
                        run_id,
                        session: run_session,
                        payload: RunTaskPayload::Chat {
                            result,
                            plan_mode: false,
                            queue_id: None,
                            agent_result_continuation_thread_ids: Vec::new(),
                        },
                    });
                });

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                });
            }
            Ok(WorkerCommand::InvokeChildSessionSkill {
                skill_id,
                arguments,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }
                if !root_config.task.enabled {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "task planning is disabled in config".to_owned(),
                    ));
                    continue;
                }

                let run_id = next_run_id;
                let loaded =
                    match load_worker_skill(&root_config, &options, &skill_id, Some(run_id)) {
                        Ok(loaded) => loaded,
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                            continue;
                        }
                    };
                if loaded.descriptor.run_as != SkillRunMode::ChildSession {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                        "skill {skill_id} is configured for {} mode, not agent mode",
                        loaded.descriptor.run_as.as_str()
                    )));
                    continue;
                }
                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let task_id = match next_task_id(&run_session) {
                    Ok(task_id) => task_id,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let task_id_value = task_id.as_str().to_owned();
                let parent_session_ref = match session_ref_for_log_path(&current_session_log_path) {
                    Ok(reference) => reference,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let objective = skill_child_session_objective(&skill_id, &arguments);
                let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                    task_id: task_id_value.clone(),
                    objective: objective.clone(),
                });

                let handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let run_id = next_run_id;
                next_run_id += 1;

                let handle = spawn_skill_child_run(
                    &runtime,
                    SkillChildRunSpawn {
                        run_id,
                        session: run_session,
                        task_id,
                        task_id_value,
                        parent_session_ref,
                        objective,
                        skill_id,
                        arguments,
                        loaded,
                        root_config: root_config.clone(),
                        options: options.clone(),
                        base_registry: agent.tool_registry().clone(),
                        agent_supervisor: agent_supervisor.clone(),
                        task_result_tx: task_result_tx.clone(),
                        approval_rx,
                        handler,
                        elicitation_audit_buffer: run_elicitation_audit_buffer,
                    },
                );

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                });
            }
            Ok(WorkerCommand::SubmitTask { prompt }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }
                if !root_config.task.enabled {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "task planning is disabled in config".to_owned(),
                    ));
                    continue;
                }

                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let task_id = match next_task_id(&run_session) {
                    Ok(task_id) => task_id,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let task_id_value = task_id.as_str().to_owned();
                let parent_session_ref = match session_ref_for_log_path(&current_session_log_path) {
                    Ok(reference) => reference,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                    task_id: task_id_value.clone(),
                    objective: prompt.clone(),
                });

                let handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let run_id = next_run_id;
                next_run_id += 1;

                let handle = spawn_task_run(
                    &runtime,
                    TaskRunSpawn {
                        run_id,
                        session: run_session,
                        task_id,
                        task_id_value,
                        parent_session_ref,
                        objective: prompt,
                        root_config: root_config.clone(),
                        options: options.clone(),
                        base_registry: agent.tool_registry().clone(),
                        agent_supervisor: agent_supervisor.clone(),
                        task_result_tx: task_result_tx.clone(),
                        approval_rx,
                        handler,
                        elicitation_audit_buffer: run_elicitation_audit_buffer,
                    },
                );

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                });
            }
            Ok(WorkerCommand::ContinueTask { task_id, guidance }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }
                if !root_config.task.enabled {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "task planning is disabled in config".to_owned(),
                    ));
                    continue;
                }

                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                let (task_id, task_id_value, objective) =
                    match resolve_continue_task(&run_session, task_id) {
                        Ok(resolved) => resolved,
                        Err(error) => {
                            current_session = Some(run_session);
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                            continue;
                        }
                    };
                let parent_session_ref = match session_ref_for_log_path(&current_session_log_path) {
                    Ok(reference) => reference,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                    task_id: task_id_value.clone(),
                    objective: objective.clone(),
                });

                let handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let run_id = next_run_id;
                next_run_id += 1;

                let handle = spawn_task_continue(
                    &runtime,
                    TaskContinueSpawn {
                        run_id,
                        session: run_session,
                        task_id,
                        task_id_value,
                        parent_session_ref,
                        objective,
                        guidance,
                        root_config: root_config.clone(),
                        options: options.clone(),
                        base_registry: agent.tool_registry().clone(),
                        agent_supervisor: agent_supervisor.clone(),
                        task_result_tx: task_result_tx.clone(),
                        approval_rx,
                        handler,
                        elicitation_audit_buffer: run_elicitation_audit_buffer,
                    },
                );

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                });
            }
            Ok(WorkerCommand::ApprovalDecision { call_id, approved }) => {
                if let Some(active_run) = &active_run {
                    let approval = if approved {
                        ToolApproval::Approve
                    } else {
                        ToolApproval::Deny {
                            reason: "denied in TUI".to_owned(),
                        }
                    };
                    let _ = active_run
                        .approval_tx
                        .send(ApprovalSignal::Decision { call_id, approval });
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "received stray approval decision without pending approval".to_owned(),
                    ));
                }
            }
            Ok(WorkerCommand::ApprovalDecisionWithArgs { call_id, args_json }) => {
                if let Some(active_run) = &active_run {
                    let _ = active_run.approval_tx.send(ApprovalSignal::Decision {
                        call_id,
                        approval: ToolApproval::ApproveWithArgs { args_json },
                    });
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "received stray approval decision without pending approval".to_owned(),
                    ));
                }
            }
            Ok(WorkerCommand::BackgroundActiveAgent) => {
                if active_run.is_none() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "no active agent run to background".to_owned(),
                    ));
                    continue;
                }
                match agent_supervisor.request_foreground_background() {
                    Ok(thread_id) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "agent {} background requested",
                            thread_id.as_str()
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "agent background unavailable: {error}"
                        )));
                    }
                }
            }
            Ok(WorkerCommand::CancelRun) => {
                if let Some(active_run) = active_run.take() {
                    cancel_active_run(
                        active_run,
                        &root_config,
                        &current_session_log_path,
                        &mut current_session,
                        &message_tx,
                        &elicitation_handler,
                        &agent_supervisor,
                        &mut discarded_run_ids,
                        "run cancelled from TUI",
                    );
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "no active run to cancel".to_owned(),
                    ));
                }
            }
            Ok(WorkerCommand::CancelTerminalTask { task_id }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before cancelling terminal task".to_owned(),
                    ));
                    continue;
                }
                match cancel_terminal_task(
                    &runtime,
                    agent.tool_registry().clone(),
                    &root_config,
                    &options,
                    &current_session_log_path,
                    &mut current_session,
                    task_id,
                ) {
                    Ok((entry, entries)) => {
                        let _ =
                            message_tx.send(WorkerMessage::TerminalTaskUpdated { entry, entries });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                }
            }
            Ok(WorkerCommand::ApprovePlan {
                plan_text,
                permission,
                scope_summary,
                clear_planning_context,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before approving a plan".to_owned(),
                    ));
                    continue;
                }
                match approve_plan(
                    &root_config,
                    &current_session_log_path,
                    &mut current_session,
                    PlanApprovalRequest {
                        plan_text,
                        permission,
                        scope_summary,
                        clear_planning_context,
                    },
                ) {
                    Ok((entry, entries)) => {
                        let _ = message_tx.send(WorkerMessage::PlanApproved { entry, entries });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                }
            }
            Ok(WorkerCommand::CloseAgent { thread_id, reason }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before closing agent".to_owned(),
                    ));
                    continue;
                }
                match close_agent_thread(
                    &root_config,
                    &current_session_log_path,
                    &mut current_session,
                    thread_id,
                    reason,
                ) {
                    Ok((thread_id, entries)) => {
                        let _ = message_tx
                            .send(WorkerMessage::AgentThreadClosed { thread_id, entries });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                }
            }
            Ok(WorkerCommand::MessageAgent { thread_id, prompt }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before messaging agent".to_owned(),
                    ));
                    continue;
                }
                match message_agent_thread(
                    &runtime,
                    &background_agent_runs,
                    &agent_supervisor,
                    &root_config,
                    agent.tool_registry(),
                    &options,
                    &mut current_session,
                    thread_id,
                    prompt,
                ) {
                    Ok((mut result, controls)) => {
                        for control in controls {
                            let _ = message_tx
                                .send(WorkerMessage::Event(Box::new(RunEvent::Control(control))));
                        }
                        result.control_entries.clear();
                        let _ = message_tx
                            .send(WorkerMessage::Event(Box::new(RunEvent::ToolResult(result))));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                }
            }
            Ok(WorkerCommand::CompactNow) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot compact while the agent is running".to_owned(),
                    ));
                    continue;
                }
                let Some(mut session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                let effective_config = effective_compaction_config(
                    session.provider_name(),
                    session.model_name(),
                    &options.compaction_config,
                );
                match session.compact_now(&effective_config) {
                    Ok(record) => {
                        current_session = Some(session);
                        if let Some(session) = current_session.as_ref() {
                            let _ = message_tx.send(session_compacted_message(
                                &current_session_log_path,
                                session,
                                record,
                                CompactionTrigger::Manual,
                            ));
                        }
                    }
                    Err(error) => {
                        current_session = Some(session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    }
                }
            }
            Ok(WorkerCommand::CheckChangedFilesDiagnostics) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot check changes while the agent is running".to_owned(),
                    ));
                    continue;
                }
                let changed_paths = match changed_source_files(&options.workspace_root) {
                    Ok(paths) => paths,
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                        continue;
                    }
                };
                if changed_paths.is_empty() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "no changed source files to check".to_owned(),
                    ));
                    continue;
                }
                let Some(session) = current_session.as_mut() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                match check_changed_files_diagnostics(
                    &runtime,
                    agent.tool_registry(),
                    session,
                    &options,
                    root_config.code_intelligence.max_results,
                    changed_paths,
                ) {
                    Ok(result) => {
                        let _ = message_tx.send(WorkerMessage::Event(Box::new(
                            diagnostics_tool_event(result),
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    }
                }
            }
            Ok(WorkerCommand::CleanMutationArtifacts { target }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before cleaning mutation artifacts".to_owned(),
                    ));
                    continue;
                }
                match clean_mutation_artifacts(
                    &root_config,
                    &current_session_log_path,
                    &current_session,
                    &target,
                ) {
                    Ok(report) => {
                        let _ = message_tx.send(WorkerMessage::Notice(
                            format_mutation_artifact_cleanup_report(&report),
                        ));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::DeleteMutationArtifact { artifact_id }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before deleting mutation artifacts".to_owned(),
                    ));
                    continue;
                }
                match delete_mutation_artifact(
                    &current_session_log_path,
                    &current_session,
                    &artifact_id,
                ) {
                    Ok(payload) => {
                        let _ = message_tx.send(WorkerMessage::Notice(
                            format_mutation_artifact_delete_report(&payload),
                        ));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::ApproveVerificationCheck { check_spec_id }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before approving verification checks".to_owned(),
                    ));
                    continue;
                }
                match promote_workspace_verification_check(
                    &options.workspace_root,
                    &root_config,
                    &mut current_session,
                    &check_spec_id,
                    VerificationCheckPromotionKind::Approve,
                ) {
                    Ok(VerificationCheckPromotionOutcome::AlreadyPromoted { check_spec_id }) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "verification check already approved: {check_spec_id}"
                        )));
                    }
                    Ok(VerificationCheckPromotionOutcome::Promoted { entry }) => {
                        let check_spec_id = entry.trusted_check.check_spec.check_spec_id.clone();
                        let _ = message_tx.send(WorkerMessage::Event(Box::new(RunEvent::Control(
                            ControlEntry::CheckSpecRecorded(*entry),
                        ))));
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "verification check approved: {check_spec_id}"
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::SandboxVerificationCheck { check_spec_id }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before sandboxing verification checks".to_owned(),
                    ));
                    continue;
                }
                match promote_workspace_verification_check(
                    &options.workspace_root,
                    &root_config,
                    &mut current_session,
                    &check_spec_id,
                    VerificationCheckPromotionKind::Sandbox,
                ) {
                    Ok(VerificationCheckPromotionOutcome::AlreadyPromoted { check_spec_id }) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "verification check already sandboxed: {check_spec_id}"
                        )));
                    }
                    Ok(VerificationCheckPromotionOutcome::Promoted { entry }) => {
                        let check_spec_id = entry.trusted_check.check_spec.check_spec_id.clone();
                        let _ = message_tx.send(WorkerMessage::Event(Box::new(RunEvent::Control(
                            ControlEntry::CheckSpecRecorded(*entry),
                        ))));
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "verification check sandboxed: {check_spec_id}"
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::RefreshProviderBalance {
                request_id,
                provider_config,
            }) => {
                provider_status_tasks.refresh_balance(
                    &runtime,
                    request_id,
                    provider_config,
                    provider_status_tx.clone(),
                );
            }
            Ok(WorkerCommand::RefreshProviderModels {
                request_id,
                provider_config,
            }) => {
                provider_status_tasks.refresh_models(
                    &runtime,
                    request_id,
                    provider_config,
                    provider_status_tx.clone(),
                );
            }
            Ok(WorkerCommand::CancelProviderModelsRefresh { request_id }) => {
                provider_status_tasks.cancel_models_refresh(request_id);
            }
            Ok(WorkerCommand::ActivateLazyMcp { server_name }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot activate MCP while the agent is running".to_owned(),
                    ));
                    continue;
                }
                let Some(agent) = Arc::get_mut(&mut agent) else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot activate MCP while agent registry is shared".to_owned(),
                    ));
                    continue;
                };
                let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                    server_name: server_name.clone(),
                    status: McpActivationStatus::Activating,
                });
                let mutation_recorder = current_session
                    .as_ref()
                    .and_then(Session::mutation_event_recorder);
                match runtime.block_on(
                    sigil_runtime::activate_lazy_mcp_tools_detailed_with_mcp_handlers_and_mutation_recorder(
                        agent.tool_registry_mut(),
                        &root_config,
                        &provider_capabilities,
                        options.workspace_root.clone(),
                        server_name.as_deref(),
                        elicitation_handler.clone(),
                        mcp_event_handler.clone(),
                        mutation_recorder,
                    ),
                ) {
                    Ok(result) if result.matched_servers == 0 => {
                        let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                            server_name: server_name.clone(),
                            status: McpActivationStatus::Deferred,
                        });
                        let detail = server_name
                            .as_deref()
                            .map(|name| format!(" for {name}"))
                            .unwrap_or_default();
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "no lazy MCP tools activated{detail}"
                        )));
                    }
                    Ok(result) => {
                        let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                            server_name: server_name.clone(),
                            status: McpActivationStatus::Ready {
                                added_tools: result.added_tools,
                            },
                        });
                        let detail = server_name
                            .as_deref()
                            .map(|name| format!(" for {name}"))
                            .unwrap_or_default();
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "activated {} lazy MCP tools{detail}",
                            result.added_tools
                        )));
                    }
                    Err(error) => {
                        let error = format!("{error:#}");
                        let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                            server_name: server_name.clone(),
                            status: McpActivationStatus::Failed {
                                error: error.clone(),
                            },
                        });
                        let detail = server_name
                            .as_deref()
                            .map(|name| format!(" for {name}"))
                            .unwrap_or_default();
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "MCP activation failed{detail}: {error}"
                        )));
                    }
                }
            }
            Ok(WorkerCommand::RefreshMcpServer { server_name }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot refresh MCP while the agent is running".to_owned(),
                    ));
                    continue;
                }
                pending_mcp_refreshes.insert(server_name);
                next_mcp_refresh_retry_at = Instant::now();
            }
            Ok(WorkerCommand::SwitchSession { session_log_path }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot switch sessions while the agent is running".to_owned(),
                    ));
                    continue;
                }

                match load_session(
                    &root_config.agent.provider,
                    &root_config.agent.model,
                    &session_log_path,
                ) {
                    Ok(mut session) => {
                        mark_stale_dispatching_conversation_queue_items(&mut session, &message_tx);
                        if current_session.as_ref().is_some_and(|session| {
                            session_workspace_is_trusted(session, &workspace_root)
                        }) {
                            match ensure_session_workspace_trust(
                                &mut session,
                                &workspace_root,
                                "trusted workspace carried into session",
                            ) {
                                Ok(()) => {}
                                Err(error) => {
                                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                                    continue;
                                }
                            }
                        }
                        let entries = session.entries().to_vec();
                        current_session_log_path = session_log_path.clone();
                        let provider_name = session.provider_name().to_owned();
                        let model_name = session.model_name().to_owned();
                        current_session = Some(session);
                        let _ = message_tx.send(WorkerMessage::SessionSwitched {
                            session_log_path,
                            provider_name,
                            model_name,
                            entries,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    }
                }
            }
            Ok(WorkerCommand::StartNewSession { session_log_path }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot start a new session while the agent is running".to_owned(),
                    ));
                    continue;
                }

                match load_session(
                    &root_config.agent.provider,
                    &root_config.agent.model,
                    &session_log_path,
                ) {
                    Ok(mut session) => {
                        mark_stale_dispatching_conversation_queue_items(&mut session, &message_tx);
                        if current_session.as_ref().is_some_and(|session| {
                            session_workspace_is_trusted(session, &workspace_root)
                        }) {
                            match ensure_session_workspace_trust(
                                &mut session,
                                &workspace_root,
                                "trusted workspace carried into new session",
                            ) {
                                Ok(()) => {}
                                Err(error) => {
                                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                                    continue;
                                }
                            }
                        }
                        let entries = session.entries().to_vec();
                        current_session_log_path = session_log_path.clone();
                        let provider_name = session.provider_name().to_owned();
                        let model_name = session.model_name().to_owned();
                        current_session = Some(session);
                        let _ = message_tx.send(WorkerMessage::NewSessionStarted {
                            session_log_path,
                            provider_name,
                            model_name,
                            entries,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    }
                }
            }
            Ok(WorkerCommand::Shutdown) => {
                if let Some(active_run) = active_run.take() {
                    elicitation_handler.set_audit_buffer(None);
                    discarded_run_ids.insert(active_run.run_id);
                    let _ = active_run.approval_tx.send(ApprovalSignal::Cancel);
                    active_run.handle.abort();
                }
                provider_status_tasks.abort_all();
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    provider_status_tasks.abort_all();
}

struct WorkerAgentEventSink {
    sender: mpsc::Sender<WorkerMessage>,
}

impl sigil_runtime::AgentToolBackgroundEventSink for WorkerAgentEventSink {
    fn handle_agent_event(&self, thread_id: &AgentThreadId, event: RunEvent) {
        let _ = self.sender.send(WorkerMessage::AgentThreadEvent {
            thread_id: thread_id.clone(),
            event: Box::new(event),
        });
    }

    fn handle_agent_status(
        &self,
        thread_id: &AgentThreadId,
        status: AgentThreadStatus,
        reason: Option<String>,
    ) {
        let _ = self.sender.send(WorkerMessage::AgentThreadStatusLive {
            entry: AgentThreadStatusChangedEntry {
                thread_id: thread_id.clone(),
                status,
                reason,
                updated_at_ms: Some(current_unix_time_ms()),
            },
        });
    }
}

pub(super) struct WorkerLoopMcpHandlers {
    pub(super) elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    pub(super) event_handler: Arc<ChannelMcpRuntimeEventHandler>,
    pub(super) event_rx: mpsc::Receiver<McpRuntimeEvent>,
}

struct ActiveRun {
    run_id: u64,
    handle: tokio::task::JoinHandle<()>,
    approval_tx: mpsc::Sender<ApprovalSignal>,
    elicitation_audit_buffer: McpElicitationAuditBuffer,
}

#[allow(clippy::too_many_arguments)]
fn cancel_active_run(
    active_run: ActiveRun,
    root_config: &RootConfig,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: &Arc<ChannelMcpElicitationHandler>,
    agent_supervisor: &sigil_runtime::AgentSupervisor,
    discarded_run_ids: &mut BTreeSet<u64>,
    reason: &str,
) {
    elicitation_handler.set_audit_buffer(None);
    discarded_run_ids.insert(active_run.run_id);
    let _ = active_run.approval_tx.send(ApprovalSignal::Cancel);
    let agent_cancel_impact = agent_supervisor.cancel_foreground_run();
    active_run.handle.abort();
    match load_session(
        &root_config.agent.provider,
        &root_config.agent.model,
        current_session_log_path,
    ) {
        Ok(session) => {
            let mut session = session;
            if let Err(error) =
                append_mcp_elicitation_audits(&mut session, &active_run.elicitation_audit_buffer)
            {
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                *current_session = Some(session);
                return;
            }
            let mut cancel_handler = ChannelEventHandler::new(message_tx.clone());
            if let Err(error) = sigil_runtime::AgentSupervisor::append_foreground_cancel_audit(
                &mut session,
                &mut cancel_handler,
                agent_cancel_impact,
                reason,
            ) {
                let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                    "failed to append cancelled agent state: {error:#}"
                )));
                *current_session = Some(session);
                return;
            }
            if let Err(error) = append_cancelled_task_state(&mut session) {
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                *current_session = Some(session);
                return;
            }
            let entries = session.entries().to_vec();
            *current_session = Some(session);
            let _ = message_tx.send(WorkerMessage::RunCancelled {
                session_log_path: current_session_log_path.to_path_buf(),
                provider_name: current_session
                    .as_ref()
                    .map(|session| session.provider_name().to_owned())
                    .unwrap_or_else(|| root_config.agent.provider.clone()),
                model_name: current_session
                    .as_ref()
                    .map(|session| session.model_name().to_owned())
                    .unwrap_or_else(|| root_config.agent.model.clone()),
                entries,
            });
        }
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
        }
    }
}

struct TaskRunSpawn {
    run_id: u64,
    session: Session,
    task_id: TaskId,
    task_id_value: String,
    parent_session_ref: SessionRef,
    objective: String,
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    task_result_tx: mpsc::Sender<RunTaskResult>,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: ChannelEventHandler,
    elicitation_audit_buffer: McpElicitationAuditBuffer,
}

struct TaskContinueSpawn {
    run_id: u64,
    session: Session,
    task_id: TaskId,
    task_id_value: String,
    parent_session_ref: SessionRef,
    objective: String,
    guidance: Option<String>,
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    task_result_tx: mpsc::Sender<RunTaskResult>,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: ChannelEventHandler,
    elicitation_audit_buffer: McpElicitationAuditBuffer,
}

struct SkillChildRunSpawn {
    run_id: u64,
    session: Session,
    task_id: TaskId,
    task_id_value: String,
    parent_session_ref: SessionRef,
    objective: String,
    skill_id: String,
    arguments: String,
    loaded: sigil_runtime::LoadedSkillContext,
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    task_result_tx: mpsc::Sender<RunTaskResult>,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: ChannelEventHandler,
    elicitation_audit_buffer: McpElicitationAuditBuffer,
}

struct TaskRoleRuntime {
    orchestrator: SequentialTaskOrchestrator<sigil_runtime::AgentSupervisorTaskChildRunner>,
    planner_options: AgentRunOptions,
    executor_options: AgentRunOptions,
    subagent_read_options: AgentRunOptions,
    subagent_write_options: AgentRunOptions,
}

fn spawn_task_run(
    runtime: &tokio::runtime::Runtime,
    spawn: TaskRunSpawn,
) -> tokio::task::JoinHandle<()> {
    runtime.spawn(async move {
        let TaskRunSpawn {
            run_id,
            mut session,
            task_id,
            task_id_value,
            parent_session_ref,
            objective,
            root_config,
            options,
            base_registry,
            agent_supervisor,
            task_result_tx,
            approval_rx,
            mut handler,
            elicitation_audit_buffer,
        } = spawn;
        let result = run_task_orchestration(
            &mut session,
            TaskRunOrchestration {
                task_id,
                parent_session_ref,
                objective,
                root_config,
                options,
                base_registry,
                agent_supervisor,
                approval_rx,
                handler: &mut handler,
            },
        )
        .await;
        let result = match append_mcp_elicitation_audits(&mut session, &elicitation_audit_buffer) {
            Ok(()) => result,
            Err(error) => Err(error),
        };
        send_task_result(run_id, session, task_id_value, result, task_result_tx);
    })
}

fn spawn_task_continue(
    runtime: &tokio::runtime::Runtime,
    spawn: TaskContinueSpawn,
) -> tokio::task::JoinHandle<()> {
    runtime.spawn(async move {
        let TaskContinueSpawn {
            run_id,
            mut session,
            task_id,
            task_id_value,
            parent_session_ref,
            objective,
            guidance,
            root_config,
            options,
            base_registry,
            agent_supervisor,
            task_result_tx,
            approval_rx,
            mut handler,
            elicitation_audit_buffer,
        } = spawn;
        let result = continue_task_orchestration(
            &mut session,
            TaskContinueOrchestration {
                task_id,
                parent_session_ref,
                objective,
                guidance,
                root_config,
                options,
                base_registry,
                agent_supervisor,
                approval_rx,
                handler: &mut handler,
            },
        )
        .await;
        let result = match append_mcp_elicitation_audits(&mut session, &elicitation_audit_buffer) {
            Ok(()) => result,
            Err(error) => Err(error),
        };
        send_task_result(run_id, session, task_id_value, result, task_result_tx);
    })
}

fn spawn_skill_child_run(
    runtime: &tokio::runtime::Runtime,
    spawn: SkillChildRunSpawn,
) -> tokio::task::JoinHandle<()> {
    runtime.spawn(async move {
        let SkillChildRunSpawn {
            run_id,
            mut session,
            task_id,
            task_id_value,
            parent_session_ref,
            objective,
            skill_id,
            arguments,
            loaded,
            root_config,
            options,
            base_registry,
            agent_supervisor,
            task_result_tx,
            approval_rx,
            mut handler,
            elicitation_audit_buffer,
        } = spawn;
        let result = run_skill_child_orchestration(
            &mut session,
            SkillChildRunOrchestration {
                task_id,
                parent_session_ref,
                objective,
                skill_id,
                arguments,
                loaded,
                root_config,
                options,
                base_registry,
                agent_supervisor,
                approval_rx,
                handler: &mut handler,
            },
        )
        .await;
        let result = match append_mcp_elicitation_audits(&mut session, &elicitation_audit_buffer) {
            Ok(()) => result,
            Err(error) => Err(error),
        };
        send_task_result(run_id, session, task_id_value, result, task_result_tx);
    })
}

struct TaskRunOrchestration<'a> {
    task_id: TaskId,
    parent_session_ref: SessionRef,
    objective: String,
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: &'a mut ChannelEventHandler,
}

struct SkillChildRunOrchestration<'a> {
    task_id: TaskId,
    parent_session_ref: SessionRef,
    objective: String,
    skill_id: String,
    arguments: String,
    loaded: sigil_runtime::LoadedSkillContext,
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: &'a mut ChannelEventHandler,
}

struct TaskContinueOrchestration<'a> {
    task_id: TaskId,
    parent_session_ref: SessionRef,
    objective: String,
    guidance: Option<String>,
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: &'a mut ChannelEventHandler,
}

async fn run_task_orchestration(
    session: &mut Session,
    request: TaskRunOrchestration<'_>,
) -> std::result::Result<TaskRunStatus, String> {
    let TaskRunOrchestration {
        task_id,
        parent_session_ref,
        objective,
        root_config,
        options,
        base_registry,
        agent_supervisor,
        approval_rx,
        handler,
    } = request;
    materialize_task_verification_config(
        session,
        handler,
        &root_config,
        &options.workspace_root,
        &task_id,
    )?;
    let TaskRoleRuntime {
        orchestrator,
        planner_options,
        executor_options,
        subagent_read_options,
        subagent_write_options,
    } = build_task_role_runtime(&root_config, &options, &base_registry, agent_supervisor)?;
    let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
    orchestrator
        .run(
            session,
            SequentialTaskRequest {
                task_id,
                parent_session_ref,
                objective,
            },
            planner_options,
            executor_options,
            subagent_read_options,
            subagent_write_options,
            root_config.task.max_plan_steps,
            handler,
            &mut approval_handler,
        )
        .await
        .map(|output| output.status)
        .map_err(|error| format!("{error:#}"))
}

async fn continue_task_orchestration(
    session: &mut Session,
    request: TaskContinueOrchestration<'_>,
) -> std::result::Result<TaskRunStatus, String> {
    let TaskContinueOrchestration {
        task_id,
        parent_session_ref,
        objective,
        guidance,
        root_config,
        options,
        base_registry,
        agent_supervisor,
        approval_rx,
        handler,
    } = request;
    materialize_task_verification_config(
        session,
        handler,
        &root_config,
        &options.workspace_root,
        &task_id,
    )?;
    let TaskRoleRuntime {
        orchestrator,
        executor_options,
        subagent_read_options,
        subagent_write_options,
        ..
    } = build_task_role_runtime(&root_config, &options, &base_registry, agent_supervisor)?;
    let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
    orchestrator
        .continue_run(
            session,
            SequentialTaskRequest {
                task_id,
                parent_session_ref,
                objective,
            },
            executor_options,
            subagent_read_options,
            subagent_write_options,
            guidance,
            handler,
            &mut approval_handler,
        )
        .await
        .map(|output| output.status)
        .map_err(|error| format!("{error:#}"))
}

async fn run_skill_child_orchestration(
    session: &mut Session,
    request: SkillChildRunOrchestration<'_>,
) -> std::result::Result<TaskRunStatus, String> {
    let SkillChildRunOrchestration {
        task_id,
        parent_session_ref,
        objective,
        skill_id,
        arguments,
        loaded,
        root_config,
        options,
        base_registry,
        agent_supervisor,
        approval_rx,
        handler,
    } = request;
    materialize_task_verification_config(
        session,
        handler,
        &root_config,
        &options.workspace_root,
        &task_id,
    )?;
    let child_role = skill_child_agent_role(&loaded.descriptor);
    let TaskRoleRuntime {
        orchestrator,
        subagent_read_options,
        subagent_write_options,
        ..
    } = build_skill_child_role_runtime(
        &root_config,
        &options,
        &base_registry,
        &loaded.descriptor,
        child_role,
        agent_supervisor,
    )?;
    session
        .append_control(ControlEntry::SkillLoaded(loaded.entry))
        .map_err(|error| format!("{error:#}"))?;
    let child_input = AgentRunInput::without_persisted_user_message(vec![
        loaded.transient_context,
        ModelMessage::user(skill_invocation_prompt(&skill_id, &arguments)),
    ]);
    let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
    orchestrator
        .run_direct_child_session(
            session,
            SequentialTaskRequest {
                task_id,
                parent_session_ref,
                objective,
            },
            TaskStepSpec {
                step_id: TaskStepId::new("invoke_skill").map_err(|error| format!("{error:#}"))?,
                title: format!("invoke agent {skill_id}"),
                display_name: Some(skill_id.clone()),
                detail: Some("direct user-invoked agent".to_owned()),
                role: child_role,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            },
            child_input,
            subagent_read_options,
            subagent_write_options,
            handler,
            &mut approval_handler,
        )
        .await
        .map(|output| output.status)
        .map_err(|error| format!("{error:#}"))
}

pub(super) fn materialize_task_verification_config(
    session: &mut Session,
    handler: &mut ChannelEventHandler,
    root_config: &RootConfig,
    workspace_root: &Path,
    task_id: &TaskId,
) -> std::result::Result<(), String> {
    let scope = EvidenceScope::Task(task_id.as_str().to_owned());
    let source_event_id = format!("config:verification:{}", task_id.as_str());
    let projection = session.verification_state_projection();
    let workspace_id = stable_workspace_id(workspace_root).map_err(|error| format!("{error:#}"))?;
    let trust_entry = projection.workspace_trust.get(&workspace_id);
    let workspace_trust_snapshot_id = trust_entry
        .map(|entry| entry.workspace_trust_snapshot_id.clone())
        .unwrap_or_else(|| format!("workspace-trust:unknown:{workspace_id}"));
    let workspace_scope = EvidenceScope::Workspace(workspace_id.clone());
    let discovered = discover_candidate_checks_with_user_config(
        workspace_root,
        workspace_trust_snapshot_id,
        source_event_id.clone(),
        &root_config.verification,
    )
    .map_err(|error| format!("{error:#}"))?;
    let mut entries = Vec::new();
    for candidate in discovered {
        let source = candidate.candidate.source;
        let candidate_source_event_id = candidate.candidate.source_event_id.clone();
        let promoted = match source {
            CheckDiscoverySource::UserExplicitConfig => {
                let promotion = CheckPromotion::ExplicitUserConfig {
                    config_event_id: source_event_id.clone(),
                };
                candidate.promote(DEFAULT_TASK_VERIFICATION_SCOPE_HASH, promotion)
            }
            _ => match workspace_promoted_check_for_candidate(
                &projection,
                &workspace_scope,
                &candidate,
            ) {
                Some(trusted) => Ok(trusted),
                None => continue,
            },
        };
        let trusted = promoted.map_err(|error| format!("{error:#}"))?;
        entries.push(sigil_kernel::CheckSpecRecordedEntry::new(
            scope.clone(),
            trusted,
            candidate_source_event_id,
        ));
    }
    if entries.is_empty() {
        return Ok(());
    }

    let projection = session.verification_state_projection();
    let mut controls = Vec::new();
    for entry in &entries {
        let check_id = entry.trusted_check.check_spec.check_spec_id.as_str();
        let needs_append = projection
            .check_spec(&scope, check_id)
            .is_none_or(|current| {
                current.trusted_check.check_spec.check_spec_hash
                    != entry.trusted_check.check_spec.check_spec_hash
            });
        if needs_append {
            controls.push(ControlEntry::CheckSpecRecorded(entry.clone()));
        }
    }

    let required_checks = entries
        .iter()
        .map(|entry| entry.trusted_check.check_spec.clone())
        .collect::<Vec<_>>();
    let workspace_trust_requirement = check_spec_entries_workspace_trust_requirement(&entries);
    let policy = VerificationPolicy {
        required_checks,
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: root_config
            .verification
            .scope_for_hash(DEFAULT_TASK_VERIFICATION_SCOPE_HASH),
        sandbox_profile: SandboxProfileRequirement::None,
        workspace_trust_requirement,
        allow_unverified_completion: false,
        timeout_ms: None,
        auto_run: root_config.verification.auto_run,
    };
    let policy_entry = VerificationPolicyChangedEntry::new(scope.clone(), policy, source_event_id)
        .map_err(|error| format!("{error:#}"))?;
    let needs_policy_append = projection
        .latest_policy(&scope)
        .is_none_or(|current| current.policy_hash != policy_entry.policy_hash);
    if needs_policy_append {
        controls.push(ControlEntry::VerificationPolicyChanged(policy_entry));
    }

    for control in controls {
        session
            .append_control(control.clone())
            .map_err(|error| format!("{error:#}"))?;
        handler
            .handle(RunEvent::Control(control))
            .map_err(|error| format!("{error:#}"))?;
    }
    Ok(())
}

fn workspace_promoted_check_for_candidate(
    projection: &sigil_kernel::VerificationStateProjection,
    workspace_scope: &EvidenceScope,
    candidate: &DiscoveredCheck,
) -> Option<sigil_kernel::TrustedCheckSpec> {
    let entry = projection.check_spec(workspace_scope, &candidate.suggested_check_spec_id)?;
    let expected = CheckSpec::new(
        candidate.suggested_check_spec_id.clone(),
        candidate.candidate.command.clone(),
        candidate.effect,
        DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
    );
    let trusted = &entry.trusted_check;
    if trusted.source != candidate.candidate.source {
        return None;
    }
    if trusted.check_spec.check_spec_hash != expected.check_spec_hash {
        return None;
    }
    Some(trusted.clone())
}

fn check_spec_entries_workspace_trust_requirement(
    entries: &[CheckSpecRecordedEntry],
) -> WorkspaceTrustRequirement {
    if entries.iter().any(|entry| {
        matches!(
            entry.trusted_check.promoted_by,
            CheckPromotion::WorkspaceTrusted { .. }
        )
    }) {
        return WorkspaceTrustRequirement::Trusted;
    }
    if entries.iter().any(|entry| {
        matches!(
            entry.trusted_check.promoted_by,
            CheckPromotion::UserApproved { .. } | CheckPromotion::Sandboxed { .. }
        )
    }) {
        return WorkspaceTrustRequirement::ApprovalOrSandbox;
    }
    WorkspaceTrustRequirement::None
}

fn session_workspace_is_trusted(session: &Session, workspace_root: &Path) -> bool {
    let Ok(workspace_id) = stable_workspace_id(workspace_root) else {
        return false;
    };
    session
        .verification_state_projection()
        .workspace_trust
        .get(&workspace_id)
        .is_some_and(|entry| entry.trust == WorkspaceTrust::Trusted)
}

fn ensure_session_workspace_trust(
    session: &mut Session,
    workspace_root: &Path,
    reason: &str,
) -> std::result::Result<(), String> {
    let workspace_id = stable_workspace_id(workspace_root).map_err(|error| format!("{error:#}"))?;
    let projection = session.verification_state_projection();
    if projection
        .workspace_trust
        .get(&workspace_id)
        .is_some_and(|entry| entry.trust == WorkspaceTrust::Trusted)
    {
        return Ok(());
    }

    let session_path = session
        .store_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "memory".to_owned());
    let seed = format!("{workspace_id}:{session_path}:{reason}");
    let digest = Sha256::digest(seed.as_bytes());
    let entry = WorkspaceTrustDecisionEntry {
        workspace_id,
        workspace_trust_snapshot_id: format!("workspace-trust:sha256:{digest:x}"),
        trust: WorkspaceTrust::Trusted,
        decided_by_event_id: None,
        reason: Some(reason.to_owned()),
    };
    session
        .append_control(ControlEntry::WorkspaceTrustDecision(entry))
        .map_err(|error| format!("failed to append workspace trust decision: {error:#}"))?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VerificationCheckPromotionKind {
    Approve,
    Sandbox,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum VerificationCheckPromotionOutcome {
    Promoted { entry: Box<CheckSpecRecordedEntry> },
    AlreadyPromoted { check_spec_id: String },
}

pub(super) fn promote_workspace_verification_check(
    workspace_root: &Path,
    root_config: &RootConfig,
    current_session: &mut Option<Session>,
    check_spec_id: &str,
    kind: VerificationCheckPromotionKind,
) -> std::result::Result<VerificationCheckPromotionOutcome, String> {
    let Some(session) = current_session.as_mut() else {
        return Err("session state is unavailable".to_owned());
    };
    let workspace_id = stable_workspace_id(workspace_root).map_err(|error| format!("{error:#}"))?;
    let projection = session.verification_state_projection();
    let trust_snapshot_id = projection
        .workspace_trust
        .get(&workspace_id)
        .map(|entry| entry.workspace_trust_snapshot_id.clone())
        .unwrap_or_else(|| format!("workspace-trust:unknown:{workspace_id}"));
    let discovered = discover_candidate_checks_with_user_config(
        workspace_root,
        trust_snapshot_id,
        "config:verification-promotion",
        &root_config.verification,
    )
    .map_err(|error| format!("{error:#}"))?;
    let Some(candidate) = discovered
        .into_iter()
        .find(|candidate| candidate.suggested_check_spec_id == check_spec_id)
    else {
        return Err(format!("verification check not found: {check_spec_id}"));
    };
    if !candidate.candidate.source.requires_trust_promotion() {
        return Err(format!(
            "verification check does not require repo-local promotion: {check_spec_id}"
        ));
    }

    let expected = CheckSpec::new(
        candidate.suggested_check_spec_id.clone(),
        candidate.candidate.command.clone(),
        candidate.effect,
        DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
    );
    let workspace_scope = EvidenceScope::Workspace(workspace_id.clone());
    if projection
        .check_spec(&workspace_scope, check_spec_id)
        .is_some_and(|entry| {
            entry.trusted_check.check_spec.check_spec_hash == expected.check_spec_hash
                && promotion_matches_kind(&entry.trusted_check.promoted_by, kind)
        })
    {
        return Ok(VerificationCheckPromotionOutcome::AlreadyPromoted {
            check_spec_id: check_spec_id.to_owned(),
        });
    }

    let sequence = session
        .next_stream_sequence_hint()
        .map_err(|error| format!("{error:#}"))?;
    let source_event_id =
        verification_check_promotion_event_id(&workspace_id, &expected, kind, sequence);
    let promotion = match kind {
        VerificationCheckPromotionKind::Approve => CheckPromotion::UserApproved {
            approval_event_id: source_event_id.clone(),
        },
        VerificationCheckPromotionKind::Sandbox => CheckPromotion::Sandboxed {
            sandbox_decision_id: source_event_id.clone(),
        },
    };
    let trusted = candidate
        .promote(DEFAULT_TASK_VERIFICATION_SCOPE_HASH, promotion)
        .map_err(|error| format!("{error:#}"))?;
    let entry = CheckSpecRecordedEntry::new(workspace_scope, trusted, source_event_id);
    session
        .append_control(ControlEntry::CheckSpecRecorded(entry.clone()))
        .map_err(|error| format!("failed to append verification check promotion: {error:#}"))?;
    Ok(VerificationCheckPromotionOutcome::Promoted {
        entry: Box::new(entry),
    })
}

fn promotion_matches_kind(
    promotion: &CheckPromotion,
    kind: VerificationCheckPromotionKind,
) -> bool {
    matches!(
        (promotion, kind),
        (
            CheckPromotion::UserApproved { .. },
            VerificationCheckPromotionKind::Approve
        ) | (
            CheckPromotion::Sandboxed { .. },
            VerificationCheckPromotionKind::Sandbox
        )
    )
}

fn verification_check_promotion_event_id(
    workspace_id: &str,
    check: &CheckSpec,
    kind: VerificationCheckPromotionKind,
    sequence: u64,
) -> String {
    let kind_label = match kind {
        VerificationCheckPromotionKind::Approve => "approve",
        VerificationCheckPromotionKind::Sandbox => "sandbox",
    };
    stable_event_uuid(
        "sigil-verification-check-promotion",
        &format!(
            "{workspace_id}:{kind_label}:{}:{}:{sequence}",
            check.check_spec_id, check.check_spec_hash
        ),
    )
}

pub(super) fn clean_mutation_artifacts(
    root_config: &RootConfig,
    current_session_log_path: &Path,
    current_session: &Option<Session>,
    target: &sigil_kernel::MutationArtifactCleanupTarget,
) -> std::result::Result<MutationArtifactRetentionReport, String> {
    if current_session.is_none() {
        return Err("session state is unavailable".to_owned());
    }
    let store = JsonlSessionStore::new(current_session_log_path)
        .map_err(|error| format!("failed to open mutation artifact recorder: {error:#}"))?;
    let recorder = MutationEventRecorder::new(store);
    recorder
        .enforce_artifact_cleanup(
            target,
            &root_config.storage.mutation_artifact_retention.to_policy(),
        )
        .map_err(|error| format!("failed to clean mutation artifacts: {error:#}"))
}

pub(super) fn delete_mutation_artifact(
    current_session_log_path: &Path,
    current_session: &Option<Session>,
    artifact_id: &str,
) -> std::result::Result<MutationArtifactLifecycleRecorded, String> {
    if current_session.is_none() {
        return Err("session state is unavailable".to_owned());
    }
    let store = JsonlSessionStore::new(current_session_log_path)
        .map_err(|error| format!("failed to open mutation artifact recorder: {error:#}"))?;
    let recorder = MutationEventRecorder::new(store);
    let event = recorder
        .delete_mutation_artifact(artifact_id.to_owned(), "user requested artifact deletion")
        .map_err(|error| format!("failed to delete mutation artifact: {error:#}"))?;
    serde_json::from_value::<MutationArtifactLifecycleRecorded>(event.payload)
        .map_err(|error| format!("failed to decode mutation artifact lifecycle: {error:#}"))
}

fn format_mutation_artifact_cleanup_report(report: &MutationArtifactRetentionReport) -> String {
    format!(
        "mutation artifact cleanup: scanned {} artifacts ({} bytes), expired {}, deleted {}, unavailable {}, recorded {} lifecycle events",
        report.scanned_artifacts,
        report.scanned_bytes,
        report.expired_artifacts,
        report.deleted_artifacts,
        report.unavailable_artifacts,
        report.lifecycle_events.len()
    )
}

fn format_mutation_artifact_delete_report(payload: &MutationArtifactLifecycleRecorded) -> String {
    let status = match payload.status {
        MutationArtifactLifecycleStatus::Deleted => "deleted",
        MutationArtifactLifecycleStatus::Expired => "expired",
        MutationArtifactLifecycleStatus::Unavailable => "unavailable",
    };
    format!(
        "mutation artifact deleted: {} status={status}",
        payload.artifact_id
    )
}

fn build_task_role_runtime(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    base_registry: &ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
) -> std::result::Result<TaskRoleRuntime, String> {
    agent_supervisor.reset_turn_budget();
    let planner_provider = sigil_runtime::build_role_provider(root_config, AgentRole::Planner)
        .map_err(|error| format!("{error:#}"))?;
    let executor_provider = sigil_runtime::build_role_provider(root_config, AgentRole::Executor)
        .map_err(|error| format!("{error:#}"))?;
    let subagent_read_provider =
        sigil_runtime::build_role_provider(root_config, AgentRole::SubagentRead)
            .map_err(|error| format!("{error:#}"))?;
    let subagent_write_provider =
        sigil_runtime::build_role_provider(root_config, AgentRole::SubagentWrite)
            .map_err(|error| format!("{error:#}"))?;
    let planner_registry =
        sigil_runtime::build_role_tool_registry(base_registry, root_config, AgentRole::Planner)
            .into_registry();
    let executor_registry =
        sigil_runtime::build_role_tool_registry(base_registry, root_config, AgentRole::Executor)
            .into_registry();
    let subagent_read_registry = sigil_runtime::build_role_tool_registry(
        base_registry,
        root_config,
        AgentRole::SubagentRead,
    )
    .into_registry();
    let subagent_write_registry = sigil_runtime::build_role_tool_registry(
        base_registry,
        root_config,
        AgentRole::SubagentWrite,
    )
    .into_registry();
    let workspace_root = options.workspace_root.clone();
    let interaction_mode = options.interaction_mode;
    let child_runner = sigil_runtime::AgentSupervisorTaskChildRunner::new(
        agent_supervisor,
        Agent::new(subagent_read_provider, subagent_read_registry),
        Agent::new(subagent_write_provider, subagent_write_registry),
    );
    let execution_backend = sigil_runtime::build_configured_execution_backend(root_config)
        .map_err(|error| format!("failed to build verification execution backend: {error:#}"))?;
    Ok(TaskRoleRuntime {
        orchestrator: SequentialTaskOrchestrator::new_with_child_runner(
            Agent::new(planner_provider, planner_registry),
            Agent::new(executor_provider, executor_registry),
            child_runner,
        )
        .with_execution_backend(execution_backend),
        planner_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root.clone(),
            interaction_mode,
            AgentRole::Planner,
        ),
        executor_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root.clone(),
            interaction_mode,
            AgentRole::Executor,
        ),
        subagent_read_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root.clone(),
            interaction_mode,
            AgentRole::SubagentRead,
        ),
        subagent_write_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root,
            interaction_mode,
            AgentRole::SubagentWrite,
        ),
    })
}

fn build_skill_child_role_runtime(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    base_registry: &ToolRegistry,
    skill: &SkillDescriptor,
    child_role: AgentRole,
    agent_supervisor: sigil_runtime::AgentSupervisor,
) -> std::result::Result<TaskRoleRuntime, String> {
    agent_supervisor.reset_turn_budget();
    let planner_provider = sigil_runtime::build_role_provider(root_config, AgentRole::Planner)
        .map_err(|error| format!("{error:#}"))?;
    let executor_provider = sigil_runtime::build_role_provider(root_config, AgentRole::Executor)
        .map_err(|error| format!("{error:#}"))?;
    let subagent_read_provider =
        sigil_runtime::build_role_provider(root_config, AgentRole::SubagentRead)
            .map_err(|error| format!("{error:#}"))?;
    let subagent_write_provider =
        sigil_runtime::build_role_provider(root_config, AgentRole::SubagentWrite)
            .map_err(|error| format!("{error:#}"))?;
    let planner_registry =
        sigil_runtime::build_role_tool_registry(base_registry, root_config, AgentRole::Planner)
            .into_registry();
    let executor_registry =
        sigil_runtime::build_role_tool_registry(base_registry, root_config, AgentRole::Executor)
            .into_registry();
    let subagent_read_registry = if child_role == AgentRole::SubagentRead {
        sigil_runtime::build_role_skill_tool_registry(
            base_registry,
            root_config,
            AgentRole::SubagentRead,
            skill,
        )
    } else {
        sigil_runtime::build_role_tool_registry(base_registry, root_config, AgentRole::SubagentRead)
    }
    .into_registry();
    let subagent_write_registry = if child_role == AgentRole::SubagentWrite {
        sigil_runtime::build_role_skill_tool_registry(
            base_registry,
            root_config,
            AgentRole::SubagentWrite,
            skill,
        )
    } else {
        sigil_runtime::build_role_tool_registry(
            base_registry,
            root_config,
            AgentRole::SubagentWrite,
        )
    }
    .into_registry();
    let workspace_root = options.workspace_root.clone();
    let interaction_mode = options.interaction_mode;
    let child_runner = sigil_runtime::AgentSupervisorTaskChildRunner::new(
        agent_supervisor,
        Agent::new(subagent_read_provider, subagent_read_registry),
        Agent::new(subagent_write_provider, subagent_write_registry),
    );
    let execution_backend = sigil_runtime::build_configured_execution_backend(root_config)
        .map_err(|error| format!("failed to build verification execution backend: {error:#}"))?;
    Ok(TaskRoleRuntime {
        orchestrator: SequentialTaskOrchestrator::new_with_child_runner(
            Agent::new(planner_provider, planner_registry),
            Agent::new(executor_provider, executor_registry),
            child_runner,
        )
        .with_execution_backend(execution_backend),
        planner_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root.clone(),
            interaction_mode,
            AgentRole::Planner,
        ),
        executor_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root.clone(),
            interaction_mode,
            AgentRole::Executor,
        ),
        subagent_read_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root.clone(),
            interaction_mode,
            AgentRole::SubagentRead,
        ),
        subagent_write_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root,
            interaction_mode,
            AgentRole::SubagentWrite,
        ),
    })
}

pub(super) fn skill_child_agent_role(skill: &SkillDescriptor) -> AgentRole {
    let Some(agent) = skill.agent.as_deref() else {
        return AgentRole::SubagentRead;
    };
    match normalized_skill_agent_hint(agent).as_str() {
        "write" | "writer" | "subagentwrite" | "subagentwriter" | "writable" => {
            AgentRole::SubagentWrite
        }
        _ => AgentRole::SubagentRead,
    }
}

fn normalized_skill_agent_hint(agent: &str) -> String {
    agent
        .chars()
        .filter(|value| value.is_ascii_alphanumeric())
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

fn load_worker_skill(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    skill_id: &str,
    run_id: Option<u64>,
) -> std::result::Result<sigil_runtime::LoadedSkillContext, String> {
    let user_config_dir = default_user_config_dir().ok();
    let sigil_paths = sigil_runtime::resolve_sigil_paths(
        &root_config.storage,
        &root_config.session,
        &options.workspace_root,
    );
    let report = sigil_runtime::discover_skill_index_with_project_assets_root(
        &options.workspace_root,
        &sigil_paths.project_assets_root,
        user_config_dir.as_deref(),
        &root_config.skills,
    )
    .map_err(|error| format!("{error:#}"))?;
    sigil_runtime::load_user_invoked_skill(
        &options.workspace_root,
        &report.snapshot,
        skill_id,
        run_id.map(|run_id| run_id.to_string()),
    )
    .map_err(|error| format!("{error:#}"))
}

fn skill_invocation_prompt(skill_id: &str, arguments: &str) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return format!(
            "Apply the loaded Sigil agent `{skill_id}` to the current task. No additional arguments were provided."
        );
    }
    format!(
        "Apply the loaded Sigil agent `{skill_id}` to the current task with these user-provided arguments:\n\n```text\n{trimmed}\n```"
    )
}

fn skill_child_session_objective(skill_id: &str, arguments: &str) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return format!("invoke agent {skill_id}");
    }
    format!("invoke agent {skill_id} with arguments: {trimmed}")
}

fn send_task_result(
    run_id: u64,
    session: Session,
    task_id: String,
    result: std::result::Result<TaskRunStatus, String>,
    task_result_tx: mpsc::Sender<RunTaskResult>,
) {
    let _ = task_result_tx.send(RunTaskResult {
        run_id,
        session,
        payload: RunTaskPayload::Task { task_id, result },
    });
}

#[allow(clippy::too_many_arguments)]
fn refresh_pending_mcp_servers<P>(
    runtime: &tokio::runtime::Runtime,
    agent: &mut Arc<Agent<P>>,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    options: &AgentRunOptions,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    mcp_event_handler: Arc<ChannelMcpRuntimeEventHandler>,
    mutation_recorder: Option<MutationEventRecorder>,
    pending_mcp_refreshes: &mut BTreeSet<String>,
) -> bool
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let servers = std::mem::take(pending_mcp_refreshes);
    let mut shared_registry_blocked = false;
    for server_name in servers {
        let Some(agent) = Arc::get_mut(agent) else {
            pending_mcp_refreshes.insert(server_name.clone());
            shared_registry_blocked = true;
            let _ = message_tx.send(WorkerMessage::RunFailed(
                "cannot refresh MCP while agent registry is shared".to_owned(),
            ));
            continue;
        };
        let _ = message_tx.send(WorkerMessage::McpActivationStatus {
            server_name: Some(server_name.clone()),
            status: McpActivationStatus::Refreshing,
        });
        let elicitation_handler_trait: Arc<dyn sigil_runtime::McpElicitationHandler> =
            elicitation_handler.clone();
        let mcp_event_handler_trait: Arc<dyn sigil_runtime::McpRuntimeEventHandler> =
            mcp_event_handler.clone();
        match runtime.block_on(
            sigil_runtime::refresh_mcp_server_tools_with_mcp_handlers_and_mutation_recorder(
                agent.tool_registry_mut(),
                root_config,
                provider_capabilities,
                options.workspace_root.clone(),
                &server_name,
                elicitation_handler_trait,
                mcp_event_handler_trait,
                mutation_recorder.clone(),
            ),
        ) {
            Ok(result) if result.matched_servers == 0 => {
                let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                    server_name: Some(server_name.clone()),
                    status: McpActivationStatus::Deferred,
                });
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "MCP refresh skipped for unknown server {server_name}"
                )));
            }
            Ok(result) => {
                let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                    server_name: Some(server_name.clone()),
                    status: McpActivationStatus::Ready {
                        added_tools: result.added_tools,
                    },
                });
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "refreshed {} MCP tools for {server_name}",
                    result.added_tools
                )));
            }
            Err(error) => {
                let error = format!("{error:#}");
                let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                    server_name: Some(server_name.clone()),
                    status: McpActivationStatus::Failed {
                        error: error.clone(),
                    },
                });
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "MCP refresh failed for {server_name}: {error}"
                )));
            }
        }
    }
    shared_registry_blocked
}

struct RunTaskResult {
    run_id: u64,
    session: Session,
    payload: RunTaskPayload,
}

enum RunTaskPayload {
    Chat {
        result: std::result::Result<AgentRunResult, String>,
        plan_mode: bool,
        queue_id: Option<ConversationInputQueueId>,
        agent_result_continuation_thread_ids: Vec<AgentThreadId>,
    },
    Agent {
        profile_id: String,
        result: std::result::Result<AgentRunResult, String>,
    },
    Task {
        task_id: String,
        result: std::result::Result<TaskRunStatus, String>,
    },
}

fn queue_conversation_input(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    prompt: String,
    kind: ConversationInputKind,
    target: ConversationInputTarget,
    reasoning_effort: ReasoningEffort,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    let entries = current_session
        .as_ref()
        .map(|session| session.entries().to_vec())
        .unwrap_or_else(|| JsonlSessionStore::read_entries(session_log_path).unwrap_or_default());
    let entry = ConversationInputQueuedEntry {
        queue_id: next_conversation_queue_id(&entries)?,
        target,
        kind,
        prompt_hash: conversation_prompt_hash(&prompt),
        prompt,
        reasoning_effort: Some(reasoning_effort),
        created_at_ms: Some(current_unix_time_ms()),
    };
    let control = ControlEntry::ConversationInputQueued(entry);
    if let Some(session) = current_session.as_mut() {
        session
            .append_control(control)
            .map_err(|error| format!("failed to append queued conversation input: {error:#}"))?;
        Ok(session.entries().to_vec())
    } else {
        let store = JsonlSessionStore::new(session_log_path.to_path_buf())
            .map_err(|error| format!("failed to open session store for queued input: {error:#}"))?;
        store
            .append(&SessionLogEntry::Control(control))
            .map_err(|error| format!("failed to persist queued conversation input: {error:#}"))?;
        JsonlSessionStore::read_entries(session_log_path)
            .map_err(|error| format!("failed to reload queued conversation input: {error:#}"))
    }
}

fn cancel_queued_conversation_input(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    queue_id: ConversationInputQueueId,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    ensure_queued_conversation_item_is_mutable(session_log_path, current_session, &queue_id)?;
    append_conversation_queue_control_entries(
        session_log_path,
        current_session,
        vec![ControlEntry::ConversationInputStatusChanged(
            ConversationInputStatusEntry {
                queue_id,
                status: ConversationInputStatus::Cancelled,
                reason: Some("cancelled by user".to_owned()),
                updated_at_ms: Some(current_unix_time_ms()),
            },
        )],
    )
}

fn edit_queued_conversation_input(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    queue_id: ConversationInputQueueId,
    prompt: String,
    reasoning_effort: ReasoningEffort,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    if prompt.trim().is_empty() {
        return Err("queued input prompt cannot be empty".to_owned());
    }
    ensure_queued_conversation_item_is_mutable(session_log_path, current_session, &queue_id)?;
    append_conversation_queue_control_entries(
        session_log_path,
        current_session,
        vec![ControlEntry::ConversationInputEdited(
            ConversationInputEditedEntry {
                queue_id,
                prompt_hash: conversation_prompt_hash(&prompt),
                prompt,
                reasoning_effort: Some(reasoning_effort),
                updated_at_ms: Some(current_unix_time_ms()),
            },
        )],
    )
}

fn move_queued_conversation_input(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    queue_id: ConversationInputQueueId,
    direction: QueueMoveDirection,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    let entries = read_conversation_queue_entries(session_log_path, current_session)?;
    let projection = ConversationQueueProjection::from_entries(&entries);
    ensure_projection_item_is_mutable(&projection, &queue_id)?;
    let Some(index) = projection
        .items
        .iter()
        .position(|item| item.queued.queue_id == queue_id)
    else {
        return Err(format!("queued input {} not found", queue_id.as_str()));
    };
    let after_queue_id = match direction {
        QueueMoveDirection::Up if index == 0 => return Ok(entries),
        QueueMoveDirection::Up if index == 1 => None,
        QueueMoveDirection::Up => Some(projection.items[index - 2].queued.queue_id.clone()),
        QueueMoveDirection::Down if index + 1 >= projection.items.len() => return Ok(entries),
        QueueMoveDirection::Down => Some(projection.items[index + 1].queued.queue_id.clone()),
    };
    append_conversation_queue_control_entries(
        session_log_path,
        current_session,
        vec![ControlEntry::ConversationInputReordered(
            ConversationInputReorderedEntry {
                queue_id,
                after_queue_id,
                updated_at_ms: Some(current_unix_time_ms()),
            },
        )],
    )
}

fn promote_queued_conversation_input(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    queue_id: ConversationInputQueueId,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    let entries = read_conversation_queue_entries(session_log_path, current_session)?;
    let projection = ConversationQueueProjection::from_entries(&entries);
    ensure_projection_item_is_mutable(&projection, &queue_id)?;
    let mut controls = Vec::new();
    if projection.paused {
        controls.push(ControlEntry::ConversationInputQueueControl(
            ConversationInputQueueControlEntry {
                action: ConversationInputQueueControlAction::Resume,
                reason: Some("next turn".to_owned()),
                updated_at_ms: Some(current_unix_time_ms()),
            },
        ));
    }
    controls.push(ControlEntry::ConversationInputReordered(
        ConversationInputReorderedEntry {
            queue_id,
            after_queue_id: None,
            updated_at_ms: Some(current_unix_time_ms()),
        },
    ));
    append_conversation_queue_control_entries(session_log_path, current_session, controls)
}

fn set_conversation_queue_paused(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    paused: bool,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    append_conversation_queue_control_entries(
        session_log_path,
        current_session,
        vec![ControlEntry::ConversationInputQueueControl(
            ConversationInputQueueControlEntry {
                action: if paused {
                    ConversationInputQueueControlAction::Pause
                } else {
                    ConversationInputQueueControlAction::Resume
                },
                reason: Some("user control".to_owned()),
                updated_at_ms: Some(current_unix_time_ms()),
            },
        )],
    )
}

fn ensure_queued_conversation_item_is_mutable(
    session_log_path: &Path,
    current_session: &Option<Session>,
    queue_id: &ConversationInputQueueId,
) -> std::result::Result<(), String> {
    let entries = read_conversation_queue_entries(session_log_path, current_session)?;
    let projection = ConversationQueueProjection::from_entries(&entries);
    ensure_projection_item_is_mutable(&projection, queue_id)
}

fn ensure_projection_item_is_mutable(
    projection: &ConversationQueueProjection,
    queue_id: &ConversationInputQueueId,
) -> std::result::Result<(), String> {
    let Some(item) = projection
        .items
        .iter()
        .find(|item| item.queued.queue_id == *queue_id)
    else {
        return Err(format!("queued input {} not found", queue_id.as_str()));
    };
    if item.status != ConversationInputStatus::Queued {
        return Err(format!(
            "queued input {} is already {}",
            queue_id.as_str(),
            queue_status_label(item.status)
        ));
    }
    Ok(())
}

fn append_conversation_queue_control_entries(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    controls: Vec<ControlEntry>,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    append_session_control_entries(
        session_log_path,
        current_session,
        controls,
        "conversation queue",
    )
    .map_err(|error| format!("{error:#}"))
}

fn append_agent_result_continuation_status_entries(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    thread_ids: &[AgentThreadId],
    status: AgentResultContinuationStatus,
    reason: Option<&str>,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    let controls = thread_ids
        .iter()
        .cloned()
        .map(|thread_id| {
            ControlEntry::AgentResultContinuation(AgentResultContinuationEntry {
                thread_id,
                status,
                reason: reason.map(str::to_owned),
                updated_at_ms: Some(current_unix_time_ms()),
            })
        })
        .collect::<Vec<_>>();
    append_session_control_entries(
        session_log_path,
        current_session,
        controls,
        "agent result continuation",
    )
    .map_err(|error| format!("{error:#}"))
}

fn append_agent_result_continuation_status_and_notify(
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    thread_ids: &[AgentThreadId],
    status: AgentResultContinuationStatus,
    reason: Option<&str>,
) {
    let Some(session) = current_session.as_mut() else {
        let _ = message_tx.send(WorkerMessage::Notice(
            "agent result continuation status skipped: session state unavailable".to_owned(),
        ));
        return;
    };
    for thread_id in thread_ids {
        let entry = AgentResultContinuationEntry {
            thread_id: thread_id.clone(),
            status,
            reason: reason.map(str::to_owned),
            updated_at_ms: Some(current_unix_time_ms()),
        };
        if let Err(error) = session.append_control(ControlEntry::AgentResultContinuation(entry)) {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "agent result continuation status append failed: {error:#}"
            )));
            return;
        }
    }
}

fn read_conversation_queue_entries(
    session_log_path: &Path,
    current_session: &Option<Session>,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    if let Some(session) = current_session.as_ref() {
        return Ok(session.entries().to_vec());
    }
    JsonlSessionStore::read_entries(session_log_path)
        .map_err(|error| format!("failed to read conversation queue state: {error:#}"))
}

fn next_conversation_queue_id(
    entries: &[SessionLogEntry],
) -> std::result::Result<ConversationInputQueueId, String> {
    let existing = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ConversationInputQueued(queued)) => {
                Some(queued.queue_id.as_str())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for index in 1..=existing.len().saturating_add(1024) {
        let candidate = format!("queue_{index}");
        if !existing.contains(candidate.as_str()) {
            return ConversationInputQueueId::new(candidate)
                .map_err(|error| format!("failed to allocate queue id: {error:#}"));
        }
    }
    Err("failed to allocate queue id".to_owned())
}

fn conversation_prompt_hash(prompt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prompt.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn queue_status_label(status: ConversationInputStatus) -> &'static str {
    match status {
        ConversationInputStatus::Queued => "queued",
        ConversationInputStatus::Dispatching => "dispatching",
        ConversationInputStatus::Delivered => "delivered",
        ConversationInputStatus::Rejected => "rejected",
        ConversationInputStatus::Cancelled => "cancelled",
        ConversationInputStatus::Stale => "stale",
        ConversationInputStatus::Unknown => "unknown",
    }
}

fn send_conversation_queue_update(
    message_tx: &mpsc::Sender<WorkerMessage>,
    entries: &[SessionLogEntry],
) {
    let projection = sigil_kernel::ConversationQueueProjection::from_entries(entries);
    let _ = message_tx.send(WorkerMessage::ConversationQueueUpdated {
        items: projection.items,
        paused: projection.paused,
        entries: entries.to_vec(),
    });
}

fn mark_stale_dispatching_conversation_queue_items(
    session: &mut Session,
    message_tx: &mpsc::Sender<WorkerMessage>,
) {
    let dispatching_queue_ids = session
        .conversation_queue_projection()
        .items
        .into_iter()
        .filter(|item| item.status == ConversationInputStatus::Dispatching)
        .map(|item| item.queued.queue_id)
        .collect::<Vec<_>>();
    if dispatching_queue_ids.is_empty() {
        return;
    }

    let mut changed = false;
    for queue_id in dispatching_queue_ids {
        let status = ConversationInputStatusEntry {
            queue_id,
            status: ConversationInputStatus::Stale,
            reason: Some("stale after session restore without active run".to_owned()),
            updated_at_ms: Some(current_unix_time_ms()),
        };
        if let Err(error) =
            session.append_control(ControlEntry::ConversationInputStatusChanged(status))
        {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "conversation queue restore skipped: {error:#}"
            )));
            break;
        }
        changed = true;
    }

    if changed {
        send_conversation_queue_update(message_tx, session.entries());
    }
}

fn append_queue_status_and_notify(
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    queue_id: ConversationInputQueueId,
    status: ConversationInputStatus,
    reason: Option<String>,
) {
    let Some(session) = current_session.as_mut() else {
        let _ = message_tx.send(WorkerMessage::Notice(
            "conversation queue status skipped: session state unavailable".to_owned(),
        ));
        return;
    };
    let entry = ConversationInputStatusEntry {
        queue_id,
        status,
        reason,
        updated_at_ms: Some(current_unix_time_ms()),
    };
    if let Err(error) = session.append_control(ControlEntry::ConversationInputStatusChanged(entry))
    {
        let _ = message_tx.send(WorkerMessage::Notice(format!(
            "conversation queue status append failed: {error:#}"
        )));
        return;
    }
    send_conversation_queue_update(message_tx, session.entries());
}

fn append_queue_failure_and_pause_and_notify(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    queue_id: ConversationInputQueueId,
    reason: String,
) {
    let controls = vec![
        ControlEntry::ConversationInputStatusChanged(ConversationInputStatusEntry {
            queue_id,
            status: ConversationInputStatus::Rejected,
            reason: Some(reason),
            updated_at_ms: Some(current_unix_time_ms()),
        }),
        ControlEntry::ConversationInputQueueControl(ConversationInputQueueControlEntry {
            action: ConversationInputQueueControlAction::Pause,
            reason: Some("queued run failed".to_owned()),
            updated_at_ms: Some(current_unix_time_ms()),
        }),
    ];
    match append_conversation_queue_control_entries(session_log_path, current_session, controls) {
        Ok(entries) => send_conversation_queue_update(message_tx, &entries),
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "conversation queue failure handling skipped: {error}"
            )));
        }
    }
}

fn mark_next_conversation_queue_item_dispatching(
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
) -> Option<ConversationInputQueuedEntry> {
    let session = current_session.as_mut()?;
    let projection = session.conversation_queue_projection();
    let queue_id = projection.next_dispatchable?;
    let queued = projection
        .items
        .iter()
        .find(|item| item.queued.queue_id == queue_id)
        .map(|item| item.queued.clone())?;
    let status = ConversationInputStatusEntry {
        queue_id,
        status: ConversationInputStatus::Dispatching,
        reason: Some("dispatching".to_owned()),
        updated_at_ms: Some(current_unix_time_ms()),
    };
    if let Err(error) = session.append_control(ControlEntry::ConversationInputStatusChanged(status))
    {
        let _ = message_tx.send(WorkerMessage::Notice(format!(
            "conversation queue dispatch skipped: {error:#}"
        )));
        return None;
    }
    send_conversation_queue_update(message_tx, session.entries());
    Some(queued)
}

fn manual_agent_invocation_result(
    invocation: &sigil_runtime::ManualAgentInvocationResult,
) -> AgentRunResult {
    let final_text = invocation
        .result
        .as_ref()
        .map(|result| result.summary.trim())
        .filter(|summary| !summary.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| match invocation.status {
            Some(AgentThreadStatus::Running) | Some(AgentThreadStatus::Started) => format!(
                "agent {} is running in background",
                invocation.thread_id.as_str()
            ),
            Some(AgentThreadStatus::Failed) => {
                format!("agent {} failed", invocation.thread_id.as_str())
            }
            Some(AgentThreadStatus::Cancelled) | Some(AgentThreadStatus::Interrupted) => {
                format!("agent {} was interrupted", invocation.thread_id.as_str())
            }
            _ => format!("agent {} completed", invocation.thread_id.as_str()),
        });
    AgentRunResult {
        final_text,
        tool_calls: 0,
        final_message_id: None,
    }
}

fn manual_agent_parent_summary(
    profile_id: &str,
    invocation: &sigil_runtime::ManualAgentInvocationResult,
) -> String {
    let status = invocation
        .status
        .map(agent_thread_status_label)
        .unwrap_or("unknown");
    let mut lines = vec![
        format!(
            "Agent @{profile_id} finished. thread_id={} status={status}.",
            invocation.thread_id.as_str()
        ),
        "Use read_agent_result for the full child answer when more detail is needed.".to_owned(),
    ];
    if let Some(result) = invocation.result.as_ref() {
        let summary = result.summary.trim();
        if !summary.is_empty() {
            lines.push(String::new());
            lines.push("Summary:".to_owned());
            lines.push(summary.to_owned());
        }
        if result.final_answer_ref.is_some() {
            lines.push(String::new());
            lines.push("Full answer is available through the agent result reference.".to_owned());
        }
    }
    lines.join("\n")
}

fn agent_thread_status_label(status: AgentThreadStatus) -> &'static str {
    match status {
        AgentThreadStatus::Started => "started",
        AgentThreadStatus::Running => "running",
        AgentThreadStatus::Blocked => "blocked",
        AgentThreadStatus::Completed => "completed",
        AgentThreadStatus::Failed => "failed",
        AgentThreadStatus::Cancelled => "cancelled",
        AgentThreadStatus::Interrupted => "interrupted",
        AgentThreadStatus::Closed => "closed",
        AgentThreadStatus::Unavailable => "unavailable",
        AgentThreadStatus::Unknown => "unknown",
    }
}

fn collect_finished_background_agent_runs(
    runtime: &tokio::runtime::Runtime,
    background_runs: &sigil_runtime::AgentToolBackgroundRuns,
    agent_supervisor: &sigil_runtime::AgentSupervisor,
    root_config: &RootConfig,
    base_registry: &ToolRegistry,
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
) -> Vec<AgentThreadId> {
    if !background_runs.has_finished() {
        return Vec::new();
    }
    let Some(session) = current_session.as_mut() else {
        return Vec::new();
    };
    let mut handler = ChannelEventHandler::new(message_tx.clone());
    let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
        agent_supervisor.clone(),
        root_config.clone(),
        base_registry.clone(),
    )
    .with_background_runs(background_runs.clone());
    match runtime.block_on(agent_delegate.collect_finished_background_runs(session, &mut handler)) {
        Ok(thread_ids) => thread_ids,
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "agent background collection failed: {error:#}"
            )));
            Vec::new()
        }
    }
}

pub(super) fn partition_agent_result_continuations(
    session: Option<&Session>,
    thread_ids: Vec<AgentThreadId>,
) -> (Vec<AgentThreadId>, Vec<AgentThreadId>) {
    let projection = session.map(Session::agent_thread_state_projection);
    let mut blocking = Vec::new();
    let mut non_blocking = Vec::new();
    for thread_id in thread_ids {
        let non_blocking_background = projection
            .as_ref()
            .and_then(|projection| projection.threads.get(&thread_id))
            .and_then(|thread| thread.invocation_mode)
            .is_some_and(|mode| mode == AgentInvocationMode::Background);
        if non_blocking_background {
            non_blocking.push(thread_id);
        } else {
            blocking.push(thread_id);
        }
    }
    (blocking, non_blocking)
}

pub(super) fn pending_agent_result_continuations_from_session(
    session: Option<&Session>,
) -> Vec<AgentThreadId> {
    session
        .map(Session::agent_result_continuation_projection)
        .map(|projection| projection.pending_thread_ids)
        .unwrap_or_default()
}

fn agent_result_continuation_new_thread_ids(
    session: Option<&Session>,
    thread_ids: &[AgentThreadId],
) -> Vec<AgentThreadId> {
    let projection = session.map(Session::agent_result_continuation_projection);
    thread_ids
        .iter()
        .filter(|thread_id| {
            projection
                .as_ref()
                .and_then(|projection| projection.statuses.get(*thread_id))
                .is_none()
        })
        .cloned()
        .collect()
}

fn extend_agent_thread_ids_unique(
    target: &mut Vec<AgentThreadId>,
    thread_ids: impl IntoIterator<Item = AgentThreadId>,
) {
    for thread_id in thread_ids {
        if !target.contains(&thread_id) {
            target.push(thread_id);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn start_agent_result_continuation_run<P>(
    runtime: &tokio::runtime::Runtime,
    agent: Arc<Agent<P>>,
    agent_supervisor: &sigil_runtime::AgentSupervisor,
    root_config: &RootConfig,
    session_log_path: &Path,
    base_registry: &ToolRegistry,
    options: &AgentRunOptions,
    background_runs: &sigil_runtime::AgentToolBackgroundRuns,
    current_session: &mut Option<Session>,
    task_result_tx: &mpsc::Sender<RunTaskResult>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    next_run_id: &mut u64,
    completed_thread_ids: Vec<AgentThreadId>,
) -> Option<ActiveRun>
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    if let Err(error) = append_agent_result_continuation_status_entries(
        session_log_path,
        current_session,
        &completed_thread_ids,
        AgentResultContinuationStatus::Started,
        Some("parent continuation started"),
    ) {
        let _ = message_tx.send(WorkerMessage::RunFailed(error));
        return None;
    }
    let Some(run_session) = current_session.take() else {
        let _ = message_tx.send(WorkerMessage::RunFailed(
            "session state is unavailable for agent result continuation".to_owned(),
        ));
        return None;
    };

    let _ = message_tx.send(WorkerMessage::AgentResultContinuationStarted {
        thread_ids: completed_thread_ids.clone(),
    });

    let mut handler = ChannelEventHandler::new(message_tx.clone());
    let (approval_tx, approval_rx) = mpsc::channel();
    let elicitation_audit_buffer: McpElicitationAuditBuffer =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
    let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
    agent_supervisor.reset_turn_budget();
    let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
        agent_supervisor.clone(),
        root_config.clone(),
        base_registry.clone(),
    )
    .with_background_runs(background_runs.clone());
    let options = options.clone();
    let task_result_tx = task_result_tx.clone();
    let run_id = *next_run_id;
    *next_run_id = (*next_run_id).saturating_add(1);
    let continuation_prompt = agent_result_continuation_prompt(&completed_thread_ids);

    let handle = runtime.spawn(async move {
        let mut run_session = run_session;
        let result = {
            let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
            agent
                .run_with_approval_input_and_agent_delegate(
                    &mut run_session,
                    AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
                        continuation_prompt,
                    )]),
                    options,
                    &mut handler,
                    &mut approval_handler,
                    &mut agent_delegate,
                )
                .await
                .map(|output| output.result)
                .map_err(|error| format!("{error:#}"))
        };
        let result =
            match append_mcp_elicitation_audits(&mut run_session, &run_elicitation_audit_buffer) {
                Ok(()) => result,
                Err(error) => Err(error),
            };
        let _ = task_result_tx.send(RunTaskResult {
            run_id,
            session: run_session,
            payload: RunTaskPayload::Chat {
                result,
                plan_mode: false,
                queue_id: None,
                agent_result_continuation_thread_ids: completed_thread_ids,
            },
        });
    });

    Some(ActiveRun {
        run_id,
        handle,
        approval_tx,
        elicitation_audit_buffer,
    })
}

fn agent_result_continuation_prompt(thread_ids: &[AgentThreadId]) -> String {
    let threads = thread_ids
        .iter()
        .map(AgentThreadId::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "One or more background child agents completed: {threads}.\n\
         Continue the original user request now. First call wait_agent for each completed thread \
         to collect its terminal status and result reference. Use read_agent_result only when the \
         bounded summary is insufficient. Do not copy the child transcript directly into the \
         parent conversation; summarize only the child result needed for the final answer."
    )
}

#[allow(clippy::too_many_arguments)]
fn start_queued_conversation_run<P>(
    runtime: &tokio::runtime::Runtime,
    agent: Arc<Agent<P>>,
    agent_supervisor: &sigil_runtime::AgentSupervisor,
    root_config: &RootConfig,
    base_registry: &ToolRegistry,
    options: &AgentRunOptions,
    background_runs: &sigil_runtime::AgentToolBackgroundRuns,
    current_session: &mut Option<Session>,
    task_result_tx: &mpsc::Sender<RunTaskResult>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    next_run_id: &mut u64,
    queued: ConversationInputQueuedEntry,
) -> Option<ActiveRun>
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    if queued.target != ConversationInputTarget::MainThread {
        append_queue_status_and_notify(
            current_session,
            message_tx,
            queued.queue_id,
            ConversationInputStatus::Rejected,
            Some("queued target is not dispatchable by the main conversation worker".to_owned()),
        );
        return None;
    }
    let plan_mode = match queued.kind {
        ConversationInputKind::Chat => false,
        ConversationInputKind::PlanPrompt => true,
        _ => {
            append_queue_status_and_notify(
                current_session,
                message_tx,
                queued.queue_id,
                ConversationInputStatus::Rejected,
                Some(
                    "queued input kind is not supported by the main conversation worker".to_owned(),
                ),
            );
            return None;
        }
    };

    let Some(run_session) = current_session.take() else {
        let _ = message_tx.send(WorkerMessage::RunFailed(
            "session state is unavailable for queued input".to_owned(),
        ));
        return None;
    };

    let ConversationInputQueuedEntry {
        queue_id,
        prompt,
        reasoning_effort,
        ..
    } = queued;
    let _ = message_tx.send(WorkerMessage::ConversationQueueDispatchStarted {
        queue_id: queue_id.clone(),
        prompt: prompt.clone(),
    });
    let background_ready_context = queued_background_ready_transient_context(Some(&run_session));

    let mut handler = ChannelEventHandler::new(message_tx.clone());
    let (approval_tx, approval_rx) = mpsc::channel();
    let elicitation_audit_buffer: McpElicitationAuditBuffer =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
    let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
    let mut options = options.clone();
    if let Some(reasoning_effort) = reasoning_effort {
        options.reasoning_effort = Some(reasoning_effort);
    }
    let delegation_requirement = agent_delegation_requirement_for_prompt(&prompt);
    agent_supervisor.reset_turn_budget();
    let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
        agent_supervisor.clone(),
        root_config.clone(),
        base_registry.clone(),
    )
    .with_background_runs(background_runs.clone());
    let plan_tools = plan_mode.then(|| {
        sigil_runtime::build_plan_prompt_tool_registry(base_registry, root_config).into_registry()
    });
    let task_result_tx = task_result_tx.clone();
    let run_id = *next_run_id;
    *next_run_id = (*next_run_id).saturating_add(1);

    let handle = runtime.spawn(async move {
        let mut run_session = run_session;
        let result = {
            let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
            let mut input = if plan_mode {
                let mut transient_context = plan_mode_transient_context(prompt);
                transient_context.extend(background_ready_context);
                AgentRunInput::without_persisted_user_message(transient_context)
            } else if background_ready_context.is_empty() {
                AgentRunInput::user(prompt)
            } else {
                AgentRunInput::transient(prompt, background_ready_context)
            };
            if let Some(requirement) = delegation_requirement {
                input = input.with_agent_delegation_requirement(requirement);
            }
            if let Some(tools) = plan_tools {
                agent
                    .run_with_approval_input_tool_registry_and_agent_delegate(
                        &mut run_session,
                        input,
                        options,
                        tools,
                        &mut handler,
                        &mut approval_handler,
                        &mut agent_delegate,
                    )
                    .await
            } else {
                agent
                    .run_with_approval_input_and_agent_delegate(
                        &mut run_session,
                        input,
                        options,
                        &mut handler,
                        &mut approval_handler,
                        &mut agent_delegate,
                    )
                    .await
            }
            .map(|output| output.result)
            .map_err(|error| format!("{error:#}"))
        };
        let result =
            match append_mcp_elicitation_audits(&mut run_session, &run_elicitation_audit_buffer) {
                Ok(()) => result,
                Err(error) => Err(error),
            };
        let _ = task_result_tx.send(RunTaskResult {
            run_id,
            session: run_session,
            payload: RunTaskPayload::Chat {
                result,
                plan_mode,
                queue_id: Some(queue_id),
                agent_result_continuation_thread_ids: Vec::new(),
            },
        });
    });

    Some(ActiveRun {
        run_id,
        handle,
        approval_tx,
        elicitation_audit_buffer,
    })
}

pub(super) fn append_mcp_elicitation_audits(
    session: &mut Session,
    audit_buffer: &McpElicitationAuditBuffer,
) -> std::result::Result<(), String> {
    let controls = {
        let mut buffer = audit_buffer
            .lock()
            .map_err(|_| "failed to lock MCP elicitation audit buffer".to_owned())?;
        std::mem::take(&mut *buffer)
    };
    for control in controls {
        if let Some(route) = subagent_elicitation_route_for_control(session, &control) {
            session
                .append_control(route)
                .map_err(|error| format!("failed to append MCP elicitation route: {error:#}"))?;
        }
        session
            .append_control(control)
            .map_err(|error| format!("failed to append MCP elicitation audit: {error:#}"))?;
    }
    Ok(())
}

fn subagent_elicitation_route_for_control(
    session: &Session,
    control: &ControlEntry,
) -> Option<ControlEntry> {
    let ControlEntry::McpElicitation(elicitation) = control else {
        return None;
    };
    let projection = session.task_state_projection();
    let task = projection.latest_task()?;
    let child = current_subagent_child(task)
        .or_else(|| latest_subagent_child_from_entries(session, task))?;
    let status = match elicitation.action {
        sigil_kernel::McpElicitationDecision::Accepted => TaskRouteStatus::Resolved,
        sigil_kernel::McpElicitationDecision::Declined => TaskRouteStatus::Rejected,
        sigil_kernel::McpElicitationDecision::Cancelled => TaskRouteStatus::Cancelled,
    };
    let route_id = TaskRouteId::new(format!(
        "route_mcp_{}",
        stable_route_suffix(&elicitation.message_hash)
    ))
    .ok()?;
    Some(ControlEntry::TaskSubagentElicitationRoute(
        TaskSubagentElicitationRouteEntry {
            route_id,
            task_id: task.task_id.clone(),
            plan_version: child.plan_version,
            step_id: child.step_id.clone(),
            role: child.role,
            child_session_ref: child.child_session_ref.clone(),
            server_name: elicitation.server_name.clone(),
            status,
        },
    ))
}

fn current_subagent_child(task: &TaskRunProjection) -> Option<TaskChildSessionEntry> {
    let (plan_version, step_id) = task.current_step.as_ref()?;
    task.child_sessions
        .values()
        .find(|child| {
            child.plan_version == *plan_version
                && child.step_id == *step_id
                && is_routable_subagent_child(child)
        })
        .cloned()
}

fn latest_subagent_child_from_entries(
    session: &Session,
    task: &TaskRunProjection,
) -> Option<TaskChildSessionEntry> {
    session.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(ControlEntry::TaskChildSession(child)) = entry else {
            return None;
        };
        if child.task_id == task.task_id && is_routable_subagent_child(child) {
            Some(child.clone())
        } else {
            None
        }
    })
}

fn is_routable_subagent_child(child: &TaskChildSessionEntry) -> bool {
    matches!(
        child.role,
        AgentRole::SubagentRead | AgentRole::SubagentWrite
    ) && child.status != TaskChildSessionStatus::Unavailable
}

fn stable_route_suffix(value: &str) -> String {
    let suffix = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(16)
        .collect::<String>();
    if suffix.is_empty() {
        "unknown".to_owned()
    } else {
        suffix
    }
}

pub(super) fn refresh_terminal_task_statuses(
    runtime: &tokio::runtime::Runtime,
    registry: &ToolRegistry,
    options: &AgentRunOptions,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
) -> std::result::Result<Vec<(TerminalTaskEntry, Vec<SessionLogEntry>)>, String> {
    let Some(session) = current_session.as_mut() else {
        return Ok(Vec::new());
    };
    let active_task_ids = session.terminal_task_projection().active_task_ids;
    if active_task_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mutation_recorder = MutationEventRecorder::new(
        JsonlSessionStore::new(current_session_log_path)
            .map_err(|error| format!("failed to open mutation recorder: {error:#}"))?,
    );
    let tool_context = ToolContext::new(options.workspace_root.clone(), options.tool_timeout_secs)
        .with_mutation_recorder(mutation_recorder.clone());
    let mut updates = Vec::new();
    for task_id in active_task_ids {
        let call = ToolCall {
            id: format!("tui-terminal-refresh-{}", task_id.as_str()),
            name: "terminal_read".to_owned(),
            args_json: serde_json::json!({
                "task_id": task_id.as_str(),
                "limit_bytes": 1
            })
            .to_string(),
        };
        let result = match runtime.block_on(registry.execute(tool_context.clone(), call)) {
            Ok(result) if !result.is_error() => result,
            Ok(_) | Err(_) => continue,
        };
        let Some(entry) = terminal_read_latest_entry(&result)? else {
            continue;
        };
        if !entry.status.is_terminal() {
            continue;
        }

        session
            .append_control(ControlEntry::TerminalTask(entry.clone()))
            .map_err(|error| format!("failed to append terminal task state: {error:#}"))?;
        if let Some(profile) =
            terminal_start_execution_profile_for_task(session.entries(), &entry.handle.task_id)
        {
            mutation_recorder
                .reconcile_execution_mutation_profile(&options.workspace_root, &profile)
                .map_err(|error| {
                    format!("failed to reconcile terminal task workspace mutation: {error:#}")
                })?;
        }
        updates.push((entry, session.entries().to_vec()));
    }
    Ok(updates)
}

pub(super) fn cancel_terminal_task(
    runtime: &tokio::runtime::Runtime,
    registry: ToolRegistry,
    root_config: &RootConfig,
    options: &AgentRunOptions,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    task_id: String,
) -> std::result::Result<(TerminalTaskEntry, Vec<SessionLogEntry>), String> {
    let mut session = load_session(
        &root_config.agent.provider,
        &root_config.agent.model,
        current_session_log_path,
    )
    .map_err(|error| format!("failed to load session before terminal cancel: {error:#}"))?;
    let terminal_task_id = TerminalTaskId::new(task_id.clone())
        .map_err(|error| format!("invalid terminal task id: {error:#}"))?;
    let projection = session.terminal_task_projection();
    let previous = projection
        .tasks
        .get(&terminal_task_id)
        .cloned()
        .ok_or_else(|| format!("terminal task {task_id} is not in the current session"))?;
    if !previous.status.is_active() {
        return Err(format!("terminal task {task_id} is not running"));
    }

    let terminal_mutation_profile =
        terminal_start_execution_profile_for_task(session.entries(), &terminal_task_id);
    let mutation_recorder = MutationEventRecorder::new(
        JsonlSessionStore::new(current_session_log_path)
            .map_err(|error| format!("failed to open mutation recorder: {error:#}"))?,
    );
    let tool_context = ToolContext::new(options.workspace_root.clone(), options.tool_timeout_secs)
        .with_mutation_recorder(mutation_recorder.clone());
    let call = ToolCall {
        id: format!("tui-terminal-cancel-{task_id}"),
        name: "terminal_cancel".to_owned(),
        args_json: serde_json::json!({ "task_id": task_id }).to_string(),
    };
    let subjects = registry
        .permission_subjects(&tool_context, &call)
        .map_err(|error| format!("invalid terminal cancel arguments: {error:#}"))?;
    let cancel_mutation_profile = registry
        .execution_mutation_profile(&tool_context, &call)
        .map_err(|error| {
            format!("failed to capture terminal cancel mutation profile: {error:#}")
        })?;
    append_terminal_cancel_execution_audit(
        &mut session,
        &call,
        &subjects,
        ToolExecutionStatus::Started,
        None,
        cancel_mutation_profile.as_ref(),
        None,
    )
    .map_err(|error| format!("failed to append terminal cancel audit: {error:#}"))?;

    let execution_started = Instant::now();
    let result = match runtime
        .block_on(registry.execute_after_started_audit(tool_context.clone(), call.clone()))
    {
        Ok(result) => result,
        Err(error) => ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::Internal,
            format!("terminal cancel failed: {error:#}"),
        ),
    };
    let duration_ms = Some(elapsed_ms(execution_started));
    let execution_status = if result.is_error() {
        ToolExecutionStatus::Failed
    } else {
        ToolExecutionStatus::Completed
    };
    append_terminal_cancel_execution_audit(
        &mut session,
        &call,
        &subjects,
        execution_status,
        duration_ms,
        None,
        Some(&result),
    )
    .map_err(|error| format!("failed to append terminal cancel audit: {error:#}"))?;
    if result.is_error() {
        *current_session = Some(session);
        return Err(format!("terminal cancel failed: {}", result.content));
    }
    let entry = terminal_cancel_entry_from_result(&previous, &result)?;
    session
        .append_control(ControlEntry::TerminalTask(entry.clone()))
        .map_err(|error| format!("failed to append terminal task state: {error:#}"))?;
    if let Some(profile) = terminal_mutation_profile {
        mutation_recorder
            .reconcile_execution_mutation_profile(&options.workspace_root, &profile)
            .map_err(|error| {
                format!("failed to reconcile terminal task workspace mutation: {error:#}")
            })?;
    }
    let entries = session.entries().to_vec();
    *current_session = Some(session);
    Ok((entry, entries))
}

fn terminal_read_latest_entry(
    result: &ToolResult,
) -> std::result::Result<Option<TerminalTaskEntry>, String> {
    let Some(details) = result.metadata.details.get("terminal_task") else {
        return Ok(None);
    };
    TerminalTaskEntry::from_tool_result_details(details)
        .map_err(|error| format!("invalid terminal read status result: {error:#}"))
}

pub(super) fn close_agent_thread(
    root_config: &RootConfig,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    thread_id: AgentThreadId,
    reason: Option<String>,
) -> std::result::Result<(AgentThreadId, Vec<SessionLogEntry>), String> {
    let mut session = load_session(
        &root_config.agent.provider,
        &root_config.agent.model,
        current_session_log_path,
    )
    .map_err(|error| format!("failed to load session before agent close: {error:#}"))?;
    let mut result = sigil_runtime::close_agent_thread(&session, thread_id.clone(), reason);
    if result.is_error() {
        *current_session = Some(session);
        return Err(format!("agent close failed: {}", result.content));
    }

    let mut closed_thread_id = None;
    for control in std::mem::take(&mut result.control_entries) {
        if let ControlEntry::AgentThreadClosed(close) = &control {
            closed_thread_id = Some(close.thread_id.clone());
        }
        session
            .append_control(control)
            .map_err(|error| format!("failed to append agent close state: {error:#}"))?;
    }
    let entries = session.entries().to_vec();
    *current_session = Some(session);
    Ok((closed_thread_id.unwrap_or(thread_id), entries))
}

#[allow(clippy::too_many_arguments)]
fn message_agent_thread(
    runtime: &tokio::runtime::Runtime,
    background_runs: &sigil_runtime::AgentToolBackgroundRuns,
    agent_supervisor: &sigil_runtime::AgentSupervisor,
    root_config: &RootConfig,
    base_registry: &ToolRegistry,
    options: &AgentRunOptions,
    current_session: &mut Option<Session>,
    thread_id: AgentThreadId,
    prompt: String,
) -> std::result::Result<(ToolResult, Vec<ControlEntry>), String> {
    let Some(session) = current_session.as_mut() else {
        return Err("session state is unavailable before agent message".to_owned());
    };
    let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
        agent_supervisor.clone(),
        root_config.clone(),
        base_registry.clone(),
    )
    .with_background_runs(background_runs.clone());
    runtime
        .block_on(agent_delegate.route_agent_message(session, thread_id, prompt, options))
        .map_err(|error| format!("agent message failed: {error:#}"))
}

pub(super) struct PlanApprovalRequest {
    pub(super) plan_text: String,
    pub(super) permission: PlanApprovalPermission,
    pub(super) scope_summary: String,
    pub(super) clear_planning_context: bool,
}

pub(super) fn approve_plan(
    root_config: &RootConfig,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    request: PlanApprovalRequest,
) -> std::result::Result<(PlanApprovedEntry, Vec<SessionLogEntry>), String> {
    let plan_text = request.plan_text.trim();
    if plan_text.is_empty() {
        return Err("plan approval failed: plan text is empty".to_owned());
    }
    let mut session = load_session(
        &root_config.agent.provider,
        &root_config.agent.model,
        current_session_log_path,
    )
    .map_err(|error| format!("failed to load session before plan approval: {error:#}"))?;
    let next_version = session
        .plan_approval_projection()
        .latest_approval
        .as_ref()
        .map(|entry| entry.plan_version.saturating_add(1))
        .unwrap_or(1);
    let scope_summary = if request.scope_summary.trim().is_empty() {
        "approved plan scope".to_owned()
    } else {
        request.scope_summary.trim().to_owned()
    };
    let workspace_paths = plan_workspace_paths(plan_text);
    let entry = PlanApprovedEntry {
        plan_version: next_version,
        plan_hash: plan_text_hash(plan_text),
        approved_at_ms: current_unix_time_ms(),
        permission: request.permission,
        scope: PlanApprovalScope {
            summary: scope_summary,
            workspace_paths,
        },
        expires: PlanApprovalExpiry::NextUserPrompt,
        clear_planning_context: request.clear_planning_context,
    };
    session
        .append_control(ControlEntry::PlanApproved(entry.clone()))
        .map_err(|error| format!("failed to append plan approval state: {error:#}"))?;
    let entries = session.entries().to_vec();
    *current_session = Some(session);
    Ok((entry, entries))
}

fn terminal_cancel_entry_from_result(
    previous: &sigil_kernel::TerminalTaskSummary,
    result: &ToolResult,
) -> std::result::Result<TerminalTaskEntry, String> {
    let entry = TerminalTaskEntry::from_tool_result_details(&result.metadata.details)
        .map_err(|error| format!("invalid terminal cancel result: {error:#}"))?
        .ok_or_else(|| "terminal cancel result did not include terminal task state".to_owned())?;
    if entry.handle.task_id != previous.handle.task_id {
        return Err(format!(
            "terminal cancel returned task {}, expected {}",
            entry.handle.task_id.as_str(),
            previous.handle.task_id.as_str()
        ));
    }
    Ok(entry)
}

fn terminal_start_execution_profile_for_task(
    entries: &[SessionLogEntry],
    task_id: &TerminalTaskId,
) -> Option<ExecutionMutationProfile> {
    let mut profiles = std::collections::BTreeMap::<String, ExecutionMutationProfile>::new();
    for entry in entries {
        let SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) = entry else {
            continue;
        };
        if execution.tool_name != "terminal_start" {
            continue;
        }
        if execution.status == ToolExecutionStatus::Started
            && let Some(profile) = execution_mutation_profile_from_details(&execution.metadata)
        {
            profiles.insert(execution.call_id.clone(), profile);
            continue;
        }
        if terminal_task_id_from_tool_metadata(&execution.metadata)
            .as_deref()
            .is_some_and(|recorded| recorded == task_id.as_str())
            && let Some(profile) = profiles.get(&execution.call_id)
        {
            return Some(profile.clone());
        }
    }
    None
}

fn execution_mutation_profile_from_details(
    metadata: &ToolResultMeta,
) -> Option<ExecutionMutationProfile> {
    metadata
        .details
        .get("execution_mutation_profile")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn terminal_task_id_from_tool_metadata(metadata: &ToolResultMeta) -> Option<String> {
    metadata
        .details
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn append_terminal_cancel_execution_audit(
    session: &mut Session,
    call: &ToolCall,
    subjects: &[ToolSubject],
    status: ToolExecutionStatus,
    duration_ms: Option<u64>,
    execution_mutation_profile: Option<&ExecutionMutationProfile>,
    result: Option<&ToolResult>,
) -> anyhow::Result<()> {
    let (changed_files, metadata, error, model_content_hash) = if let Some(result) = result {
        let error = match &result.status {
            ToolResultStatus::Ok => None,
            ToolResultStatus::Error(error) => Some(error.clone()),
        };
        (
            result.metadata.changed_files.clone(),
            result.metadata.clone(),
            error,
            Some(tool_result_model_content_hash(result)),
        )
    } else {
        let mut details = serde_json::json!({
            "call": {
                "summary": format!("task_id={}", terminal_cancel_task_id_from_call(call))
            }
        });
        if let Some(profile) = execution_mutation_profile {
            details["execution_mutation_profile"] = serde_json::to_value(profile)?;
        }
        (
            Vec::new(),
            ToolResultMeta {
                details,
                ..ToolResultMeta::default()
            },
            None,
            None,
        )
    };
    session.append_control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        status,
        duration_ms,
        subjects: subjects.iter().map(ToolSubjectAudit::from).collect(),
        changed_files,
        metadata,
        error,
        model_content_hash,
    })))
}

fn terminal_cancel_task_id_from_call(call: &ToolCall) -> String {
    serde_json::from_str::<serde_json::Value>(&call.args_json)
        .ok()
        .and_then(|value| {
            value
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

fn tool_result_model_content_hash(result: &ToolResult) -> String {
    let mut hasher = Sha256::new();
    hasher.update(result.to_model_content().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(saturating_elapsed(started).as_millis()).unwrap_or(u64::MAX)
}

pub(super) fn append_cancelled_task_state(
    session: &mut Session,
) -> std::result::Result<(), String> {
    let projection = session.task_state_projection();
    let Some(task) = projection.latest_task() else {
        return Ok(());
    };
    if !matches!(task.status, TaskRunStatus::Started | TaskRunStatus::Running) {
        return Ok(());
    }
    let task_id = task.task_id.clone();
    let parent_session_ref = task.parent_session_ref.clone();
    let objective = task.objective.clone();
    let current_step = task.current_step.clone().and_then(|key| {
        task.steps.get(&key).and_then(|step| {
            if step.status.is_terminal() {
                None
            } else {
                Some(step.clone())
            }
        })
    });
    let child_cancellations = task
        .child_sessions
        .values()
        .filter(|child| child.status == TaskChildSessionStatus::Started)
        .cloned()
        .collect::<Vec<_>>();
    let _ = task;

    if let Some(step) = current_step {
        session
            .append_control(ControlEntry::TaskStep(TaskStepEntry {
                task_id: task_id.clone(),
                plan_version: step.plan_version,
                step_id: step.step_id,
                role: step.role,
                status: TaskStepStatus::Cancelled,
                title: step.title,
                summary: None,
                reason: Some("run cancelled from TUI".to_owned()),
            }))
            .map_err(|error| format!("failed to append cancelled task step: {error:#}"))?;
    }
    for mut child in child_cancellations {
        child.status = TaskChildSessionStatus::Cancelled;
        session
            .append_control(ControlEntry::TaskChildSession(child))
            .map_err(|error| format!("failed to append cancelled child session: {error:#}"))?;
    }
    session
        .append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id,
            parent_session_ref,
            objective,
            status: TaskRunStatus::Cancelled,
            reason: Some("run cancelled from TUI".to_owned()),
        }))
        .map_err(|error| format!("failed to append cancelled task run: {error:#}"))?;
    Ok(())
}

fn session_ref_for_log_path(path: &Path) -> std::result::Result<SessionRef, String> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("session.jsonl");
    SessionRef::new_relative(file_name)
        .map_err(|error| format!("failed to build parent session ref: {error:#}"))
}

fn plan_mode_transient_context(prompt: String) -> Vec<ModelMessage> {
    vec![
        ModelMessage::system(
            "Plan mode is active for this turn. Research, inspect, and propose a concrete plan, but do not modify files, run write-capable tools, or execute the plan. Use read-only tools and read-only agent delegation when helpful. End with the plan and any open questions needed before implementation.",
        ),
        ModelMessage::user(prompt),
    ]
}

pub(super) fn queued_background_ready_transient_context(
    session: Option<&Session>,
) -> Vec<ModelMessage> {
    const MAX_READY_THREAD_IDS: usize = 5;
    let Some(session) = session else {
        return Vec::new();
    };
    let mut thread_ids = session
        .agent_result_continuation_projection()
        .pending_thread_ids
        .into_iter()
        .map(|thread_id| thread_id.as_str().to_owned())
        .collect::<Vec<_>>();
    if thread_ids.is_empty() {
        return Vec::new();
    }
    let hidden_count = thread_ids.len().saturating_sub(MAX_READY_THREAD_IDS);
    thread_ids.truncate(MAX_READY_THREAD_IDS);
    let hidden_suffix = if hidden_count == 0 {
        String::new()
    } else {
        format!(" and {hidden_count} more")
    };
    vec![ModelMessage::system(format!(
        "Background agent result ready notice: child agent results are available for {}{}. This notice is transient and does not preempt the queued user input. Continue the queued user request first; call wait_agent/read_agent_result only if those background results are relevant.",
        thread_ids.join(", "),
        hidden_suffix
    ))]
}

pub(super) fn agent_delegation_requirement_for_prompt(
    prompt: &str,
) -> Option<AgentDelegationRequirement> {
    let normalized = prompt.to_lowercase().replace(
        ['\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}'],
        "-",
    );
    let compact = normalized
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let mentions_subagent = normalized.contains("subagent")
        || normalized.contains("sub-agent")
        || normalized.contains("sub agent")
        || compact.contains("子agent")
        || compact.contains("子代理");
    if !mentions_subagent {
        return None;
    }
    let compact_negations = [
        "不要用子agent",
        "不用子agent",
        "别用子agent",
        "不要使用子agent",
        "不使用子agent",
        "不需要子agent",
        "无需子agent",
        "别开子agent",
        "不要开子agent",
        "不要启动子agent",
    ];
    let normalized_negations = [
        "do not use subagent",
        "do not use sub-agent",
        "don't use subagent",
        "don't use sub-agent",
        "don't use a subagent",
        "don't use a sub-agent",
        "without subagent",
        "without sub-agent",
        "without a subagent",
        "without a sub-agent",
        "do not spawn subagent",
        "do not spawn a sub-agent",
    ];
    let negated = compact_negations
        .iter()
        .any(|phrase| compact.contains(phrase))
        || normalized_negations
            .iter()
            .any(|phrase| normalized.contains(phrase));
    if negated {
        return None;
    }
    let explicit = compact.contains("必须用子agent")
        || compact.contains("必须使用子agent")
        || compact.contains("同时用子agent")
        || compact.contains("让子agent")
        || compact.contains("用子agent")
        || compact.contains("使用子agent")
        || normalized.contains("must use subagent")
        || normalized.contains("must use sub-agent")
        || normalized.contains("use a subagent")
        || normalized.contains("use a sub-agent")
        || normalized.contains("use subagent")
        || normalized.contains("use sub-agent")
        || normalized.contains("delegate to a subagent")
        || normalized.contains("delegate to a sub-agent");
    explicit.then(|| {
        AgentDelegationRequirement::new(
            "the user explicitly requested sub-agent delegation in the current prompt",
        )
    })
}

pub(super) fn next_task_id(session: &Session) -> std::result::Result<TaskId, String> {
    let projection = session.task_state_projection();
    let mut counter = 1usize;
    loop {
        let value = format!("task_{counter}");
        let task_id = TaskId::new(value.clone())
            .map_err(|error| format!("failed to build next task id: {error:#}"))?;
        if !projection.tasks.contains_key(&task_id) {
            return Ok(task_id);
        }
        counter = counter.saturating_add(1);
    }
}

pub(super) fn resolve_continue_task(
    session: &Session,
    requested_task_id: Option<String>,
) -> std::result::Result<(TaskId, String, String), String> {
    let projection = session.task_state_projection();
    let task = match requested_task_id {
        Some(value) => {
            let task_id = TaskId::new(value.clone())
                .map_err(|error| format!("invalid task id for continue: {error:#}"))?;
            projection
                .tasks
                .get(&task_id)
                .ok_or_else(|| format!("task {value} is not present in this session"))?
        }
        None => projection
            .latest_unfinished_task()
            .or_else(|| projection.latest_task())
            .ok_or_else(|| "no task is available to continue".to_owned())?,
    };
    match task.status {
        TaskRunStatus::Completed => {
            return Err(format!(
                "task {} is already completed",
                task.task_id.as_str()
            ));
        }
        TaskRunStatus::Cancelled => {
            return Err(format!("task {} is cancelled", task.task_id.as_str()));
        }
        TaskRunStatus::Started
        | TaskRunStatus::Running
        | TaskRunStatus::Paused
        | TaskRunStatus::Failed
        | TaskRunStatus::Interrupted => {}
    }
    if task.latest_plan_version.is_none() {
        return Err(format!(
            "task {} has no plan to continue",
            task.task_id.as_str()
        ));
    }
    Ok((
        task.task_id.clone(),
        task.task_id.as_str().to_owned(),
        task.objective.clone(),
    ))
}
