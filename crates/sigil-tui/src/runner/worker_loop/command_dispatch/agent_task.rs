use super::*;

pub(super) fn dispatch_agent_task_command<P>(
    context: WorkerCommandContext<'_, P>,
    command: AgentTaskCommand,
) -> WorkerCommandDispatchControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerCommandContext {
        runtime,
        agent,
        root_config,
        provider_capabilities: _,
        workspace_root,
        options,
        message_tx,
        elicitation_handler,
        mcp_event_handler: _,
        role_provider_builder,
        context_resolver: _,
        state,
    } = context;
    let mut command_result = Some(command);
    let control = WorkerCommandDispatchControl::Continue;
    while let Some(command_result) = command_result.take() {
        match command_result {
            AgentTaskCommand::InvokeAgentProfile {
                profile_id,
                prompt,
                parent_prompt,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }

                let Some(run_session) = state.session.current.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                let mut run_session = run_session;
                let safe_parent_prompt = sigil_kernel::safe_persistence_text(&parent_prompt);
                if let Err(error) =
                    run_session.append_user_message(ModelMessage::user(safe_parent_prompt.clone()))
                {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                        "failed to persist agent invocation prompt: {error:#}"
                    )));
                    state.session.current = Some(run_session);
                    continue;
                }

                let _ = message_tx.send(WorkerMessage::AgentRunStarted {
                    profile_id: profile_id.clone(),
                    prompt: safe_parent_prompt,
                });

                let mut handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
                    state.agent.supervisor.clone(),
                    root_config.clone(),
                    agent.tool_registry().clone(),
                )
                .with_background_runs(state.agent.background_runs.clone());
                let options = options.clone();
                let task_result_tx = state.run.result_tx.clone();
                let run_id = state.run.next_id;
                state.run.next_id += 1;
                let cancellation_recorder = match run_session.run_cancellation_recorder() {
                    Ok(recorder) => recorder,
                    Err(error) => {
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                            "failed to create cancellation recorder: {error}"
                        )));
                        continue;
                    }
                };
                let cancellation_owner = RunCancellationOwner::new();
                let cancellation_handle = cancellation_owner.handle();
                let run_task_guard = cancellation_handle
                    .register_task()
                    .expect("new root cancellation owner must admit its first task");
                sigil_kernel::AgentToolDelegate::set_run_cancellation(
                    &mut agent_delegate,
                    Some(cancellation_handle.clone()),
                );

                let url_capability_registrar = run_session.user_url_capability_registrar();
                let image_attachment_resolver = run_session.image_attachment_resolver();
                let handle = runtime.spawn(async move {
                    let _run_task_guard = run_task_guard;
                    let profile_id_for_summary = profile_id.clone();
                    let terminal_cancellation = cancellation_handle.clone();
                    let result = async {
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
                    let result = result.await;
                    let result = if terminal_cancellation.try_finalize_naturally() {
                        result
                    } else {
                        Err("run cancellation won the manual agent terminal-state race".to_owned())
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

                state.run.active = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                    cancellation_owner,
                    cancellation_recorder,
                    url_capability_registrar,
                    image_attachment_resolver,
                });
            }
            AgentTaskCommand::InvokeChildSessionSkill {
                skill_id,
                arguments,
            } => {
                if state.run.active.is_some() {
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

                let run_id = state.run.next_id;
                let loaded = match load_worker_skill(root_config, options, &skill_id, Some(run_id))
                {
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
                let Some(run_session) = state.session.current.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let task_id = match next_task_id(&run_session) {
                    Ok(task_id) => task_id,
                    Err(error) => {
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let task_id_value = task_id.as_str().to_owned();
                let parent_session_ref = match session_ref_for_log_path(&state.session.log_path) {
                    Ok(reference) => reference,
                    Err(error) => {
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let objective = skill_child_session_objective(&skill_id, &arguments);
                let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                    task_id: task_id_value.clone(),
                    objective: sigil_kernel::safe_persistence_text(&objective),
                });

                let handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let run_id = state.run.next_id;
                state.run.next_id += 1;
                let (
                    cancellation_owner,
                    cancellation_recorder,
                    cancellation_handle,
                    cancellation_task_guard,
                ) = match prepare_run_cancellation(&run_session) {
                    Ok(cancellation) => cancellation,
                    Err(error) => {
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };

                let url_capability_registrar = run_session.user_url_capability_registrar();
                let image_attachment_resolver = run_session.image_attachment_resolver();
                let handle = spawn_skill_child_run(
                    runtime,
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
                        agent_supervisor: state.agent.supervisor.clone(),
                        role_provider_builder: Arc::clone(role_provider_builder),
                        task_result_tx: state.run.result_tx.clone(),
                        approval_rx,
                        handler,
                        elicitation_audit_buffer: run_elicitation_audit_buffer,
                        cancellation_handle,
                        cancellation_task_guard,
                    },
                );

                state.run.active = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                    cancellation_owner,
                    cancellation_recorder,
                    url_capability_registrar,
                    image_attachment_resolver,
                });
            }
            AgentTaskCommand::SubmitTask { prompt } => {
                if state.run.active.is_some() {
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

                let Some(run_session) = state.session.current.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let task_id = match next_task_id(&run_session) {
                    Ok(task_id) => task_id,
                    Err(error) => {
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let task_id_value = task_id.as_str().to_owned();
                let parent_session_ref = match session_ref_for_log_path(&state.session.log_path) {
                    Ok(reference) => reference,
                    Err(error) => {
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                    task_id: task_id_value.clone(),
                    objective: sigil_kernel::safe_persistence_text(&prompt),
                });

                let handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let run_id = state.run.next_id;
                state.run.next_id += 1;
                let (
                    cancellation_owner,
                    cancellation_recorder,
                    cancellation_handle,
                    cancellation_task_guard,
                ) = match prepare_run_cancellation(&run_session) {
                    Ok(cancellation) => cancellation,
                    Err(error) => {
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };

                let url_capability_registrar = run_session.user_url_capability_registrar();
                let image_attachment_resolver = run_session.image_attachment_resolver();
                let handle = spawn_task_run(
                    runtime,
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
                        agent_supervisor: state.agent.supervisor.clone(),
                        role_provider_builder: Arc::clone(role_provider_builder),
                        task_result_tx: state.run.result_tx.clone(),
                        approval_rx,
                        handler,
                        elicitation_audit_buffer: run_elicitation_audit_buffer,
                        cancellation_handle,
                        cancellation_task_guard,
                    },
                );

                state.run.active = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                    cancellation_owner,
                    cancellation_recorder,
                    url_capability_registrar,
                    image_attachment_resolver,
                });
            }
            AgentTaskCommand::ContinueTask { task_id, guidance } => {
                if state.run.active.is_some() {
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

                let Some(run_session) = state.session.current.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                let (task_id, task_id_value, objective) =
                    match resolve_continue_task(&run_session, task_id) {
                        Ok(resolved) => resolved,
                        Err(error) => {
                            state.session.current = Some(run_session);
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                            continue;
                        }
                    };
                let parent_session_ref = match session_ref_for_log_path(&state.session.log_path) {
                    Ok(reference) => reference,
                    Err(error) => {
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                    task_id: task_id_value.clone(),
                    objective: sigil_kernel::safe_persistence_text(&objective),
                });

                let handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let run_id = state.run.next_id;
                state.run.next_id += 1;
                let (
                    cancellation_owner,
                    cancellation_recorder,
                    cancellation_handle,
                    cancellation_task_guard,
                ) = match prepare_run_cancellation(&run_session) {
                    Ok(cancellation) => cancellation,
                    Err(error) => {
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };

                let url_capability_registrar = run_session.user_url_capability_registrar();
                let image_attachment_resolver = run_session.image_attachment_resolver();
                let handle = spawn_task_continue(
                    runtime,
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
                        agent_supervisor: state.agent.supervisor.clone(),
                        role_provider_builder: Arc::clone(role_provider_builder),
                        task_result_tx: state.run.result_tx.clone(),
                        approval_rx,
                        handler,
                        elicitation_audit_buffer: run_elicitation_audit_buffer,
                        cancellation_handle,
                        cancellation_task_guard,
                    },
                );

                state.run.active = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                    cancellation_owner,
                    cancellation_recorder,
                    url_capability_registrar,
                    image_attachment_resolver,
                });
            }
            AgentTaskCommand::BackgroundActiveAgent => {
                if state.run.active.is_none() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "no active agent run to background".to_owned(),
                    ));
                    continue;
                }
                match state.agent.supervisor.request_foreground_background() {
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
            AgentTaskCommand::CancelTerminalTask { task_id } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before cancelling terminal task".to_owned(),
                    ));
                    continue;
                }
                match cancel_terminal_task(
                    runtime,
                    agent.tool_registry().clone(),
                    root_config,
                    options,
                    &state.session.log_path,
                    &mut state.session.current,
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
            AgentTaskCommand::CreateTaskFromPlan {
                plan_id,
                expected_plan_hash,
                start_mode,
                permission_grant,
            } => {
                if state.run.active.is_some() {
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
                    root_config,
                    workspace_root,
                    &state.session.log_path,
                    &mut state.session.current,
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

                let Some(run_session) = state.session.current.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                let parent_session_ref = match session_ref_for_log_path(&state.session.log_path) {
                    Ok(reference) => reference,
                    Err(error) => {
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                    task_id: created.task_id_value.clone(),
                    objective: sigil_kernel::safe_persistence_text(&created.objective),
                });
                let handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let run_id = state.run.next_id;
                state.run.next_id += 1;
                let (
                    cancellation_owner,
                    cancellation_recorder,
                    cancellation_handle,
                    cancellation_task_guard,
                ) = match prepare_run_cancellation(&run_session) {
                    Ok(cancellation) => cancellation,
                    Err(error) => {
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let url_capability_registrar = run_session.user_url_capability_registrar();
                let image_attachment_resolver = run_session.image_attachment_resolver();
                let handle = spawn_task_run(
                    runtime,
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
                        agent_supervisor: state.agent.supervisor.clone(),
                        role_provider_builder: Arc::clone(role_provider_builder),
                        task_result_tx: state.run.result_tx.clone(),
                        approval_rx,
                        handler,
                        elicitation_audit_buffer: run_elicitation_audit_buffer,
                        cancellation_handle,
                        cancellation_task_guard,
                    },
                );
                state.run.active = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                    cancellation_owner,
                    cancellation_recorder,
                    url_capability_registrar,
                    image_attachment_resolver,
                });
            }
            AgentTaskCommand::CloseAgent { thread_id, reason } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before closing agent".to_owned(),
                    ));
                    continue;
                }
                match close_agent_thread(
                    root_config,
                    &state.session.log_path,
                    &mut state.session.current,
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
            AgentTaskCommand::CancelAgent { thread_id, reason } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before cancelling agent".to_owned(),
                    ));
                    continue;
                }
                match cancel_agent_thread(
                    runtime,
                    &state.agent.background_runs,
                    &state.agent.supervisor,
                    root_config,
                    agent.tool_registry(),
                    options,
                    &mut state.session.current,
                    thread_id,
                    reason,
                ) {
                    Ok((thread_id, entries)) => {
                        let _ = message_tx
                            .send(WorkerMessage::AgentThreadCancelled { thread_id, entries });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                }
            }
            AgentTaskCommand::MessageAgent { thread_id, prompt } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before messaging agent".to_owned(),
                    ));
                    continue;
                }
                match message_agent_thread(
                    runtime,
                    &state.agent.background_runs,
                    &state.agent.supervisor,
                    root_config,
                    agent.tool_registry(),
                    options,
                    &mut state.session.current,
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
        }
    }
    control
}
