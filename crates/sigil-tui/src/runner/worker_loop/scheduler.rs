use super::agent_runtime::chat_agent_run_input_with_repo_context;
use super::*;

pub(in crate::runner) fn run_worker_loop<P>(
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
        role_provider_builder,
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
    let mut processed_worker_command_ids = BTreeSet::<String>::new();
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

        drain_provider_status_results(&provider_status_rx, &mut provider_status_tasks, &message_tx);

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
                    if plan_mode
                        && let Err(error) = append_plan_draft(
                            &root_config,
                            &workspace_root,
                            &current_session_log_path,
                            &mut current_session,
                            &run_result.final_text,
                            run_result.final_message_id.clone(),
                            task_result.run_id,
                        )
                    {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
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
                                "run interrupted for follow-up",
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
                let workspace_root = options.workspace_root.clone();

                let handle = runtime.spawn(async move {
                    let mut run_session = run_session;
                    let result = {
                        let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
                        let input = chat_agent_run_input_with_repo_context(
                            &workspace_root,
                            prompt,
                            plan_mode,
                            Vec::new(),
                        );
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
                        role_provider_builder: Arc::clone(&role_provider_builder),
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
                        role_provider_builder: Arc::clone(&role_provider_builder),
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
                        role_provider_builder: Arc::clone(&role_provider_builder),
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
            Ok(WorkerCommand::ApprovalSessionDecision { call_id }) => {
                if let Some(active_run) = &active_run {
                    let _ = active_run.approval_tx.send(ApprovalSignal::Decision {
                        call_id,
                        approval: ToolApproval::ApproveForSession,
                    });
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "received stray approval decision without pending approval".to_owned(),
                    ));
                }
            }
            Ok(WorkerCommand::ApprovalCommand(command)) => {
                if processed_worker_command_ids.contains(&command.command_id) {
                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                        "duplicate command {} ignored",
                        command.command_id
                    )));
                    continue;
                }
                let signal = match command.payload {
                    WorkerApprovalCommand::Decision { call_id, approved } => {
                        let approval = if approved {
                            ToolApproval::Approve
                        } else {
                            ToolApproval::Deny {
                                reason: "denied in TUI".to_owned(),
                            }
                        };
                        ApprovalSignal::Decision { call_id, approval }
                    }
                    WorkerApprovalCommand::DecisionForSession { call_id } => {
                        ApprovalSignal::Decision {
                            call_id,
                            approval: ToolApproval::ApproveForSession,
                        }
                    }
                    WorkerApprovalCommand::DecisionWithArgs { call_id, args_json } => {
                        ApprovalSignal::Decision {
                            call_id,
                            approval: ToolApproval::ApproveWithArgs { args_json },
                        }
                    }
                };
                if let Some(active_run) = &active_run {
                    if active_run.approval_tx.send(signal).is_ok() {
                        processed_worker_command_ids.insert(command.command_id);
                    } else {
                        let _ = message_tx.send(WorkerMessage::RunFailed(
                            "approval channel closed".to_owned(),
                        ));
                    }
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "received stray approval command without pending approval".to_owned(),
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
            Ok(WorkerCommand::CreateTaskFromPlan {
                plan_id,
                expected_plan_hash,
                start_mode,
                permission_grant,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before creating a task from a plan".to_owned(),
                    ));
                    continue;
                }
                if !root_config.task.enabled {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "task planning is disabled in config".to_owned(),
                    ));
                    continue;
                }
                let created = match create_task_from_plan(
                    &root_config,
                    &workspace_root,
                    &current_session_log_path,
                    &mut current_session,
                    CreateTaskFromPlanRequest {
                        plan_id,
                        expected_plan_hash,
                        start_mode,
                        permission_grant,
                    },
                ) {
                    Ok(created) => created,
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                        continue;
                    }
                };
                let _ = message_tx.send(WorkerMessage::TaskCreatedFromPlan {
                    entry: created.entry.clone(),
                    start_mode: created.start_mode,
                    entries: created.entries.clone(),
                });
                if created.start_mode == PlanTaskStartMode::CreatePaused {
                    continue;
                }

                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
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
                    task_id: created.task_id_value.clone(),
                    objective: created.objective.clone(),
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
                        task_id: created.task_id,
                        task_id_value: created.task_id_value,
                        parent_session_ref,
                        objective: created.objective,
                        root_config: root_config.clone(),
                        options: options.clone(),
                        base_registry: agent.tool_registry().clone(),
                        agent_supervisor: agent_supervisor.clone(),
                        role_provider_builder: Arc::clone(&role_provider_builder),
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
            Ok(WorkerCommand::RejectPlan {
                plan_id,
                expected_plan_hash,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before rejecting a plan".to_owned(),
                    ));
                    continue;
                }
                match reject_plan(
                    &root_config,
                    &current_session_log_path,
                    &mut current_session,
                    RejectPlanRequest {
                        plan_id,
                        expected_plan_hash,
                    },
                ) {
                    Ok((entry, entries)) => {
                        let _ = message_tx.send(WorkerMessage::PlanRejected { entry, entries });
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
                                process_coverage: sigil_runtime::mcp_process_receipts_summary(
                                    &result.process_launch_receipts,
                                ),
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
