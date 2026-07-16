use super::super::agent_runtime::chat_agent_run_input_with_repo_context;
use super::*;

pub(super) fn dispatch_run_plan_command<P>(
    context: WorkerCommandContext<'_, P>,
    command: RunPlanCommand,
) -> WorkerCommandDispatchControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerCommandContext {
        runtime,
        agent,
        root_config,
        provider_capabilities: _,
        workspace_root: _,
        options,
        message_tx,
        elicitation_handler,
        mcp_event_handler: _,
        role_provider_builder: _,
        context_resolver,
        state,
    } = context;
    let mut command_result = Some(command);
    let control = WorkerCommandDispatchControl::Continue;
    while let Some(command_result) = command_result.take() {
        match command_result {
            RunPlanCommand::Submit {
                prompt,
                attachments,
                reasoning_effort,
                plan_mode,
            } => {
                if state.run.active.is_some() {
                    if !attachments.is_empty() {
                        let _ = message_tx.send(WorkerMessage::RunFailed(
                            "image attachments cannot be queued; wait for the active run"
                                .to_owned(),
                        ));
                        continue;
                    }
                    let kind = if plan_mode {
                        ConversationInputKind::PlanPrompt
                    } else {
                        ConversationInputKind::Chat
                    };
                    match queue_conversation_input(
                        &state.session.log_path,
                        &mut state.session.current,
                        &mut state.session.exact_prompts,
                        prompt,
                        kind,
                        ConversationInputTarget::MainThread,
                        reasoning_effort,
                    ) {
                        Ok(entries) => send_conversation_queue_update(message_tx, &entries),
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        }
                    }
                    continue;
                }

                let Some(run_session) = state.session.current.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let safe_started_prompt = if prompt.is_empty() && !attachments.is_empty() {
                    sigil_kernel::render_image_attachment_placeholders(&attachments)
                } else {
                    sigil_kernel::safe_persistence_text(&prompt)
                };
                let started = if plan_mode {
                    WorkerMessage::PlanRunStarted {
                        prompt: safe_started_prompt,
                    }
                } else {
                    WorkerMessage::RunStarted {
                        prompt: safe_started_prompt,
                    }
                };
                let _ = message_tx.send(started);

                let mut handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let agent = Arc::clone(agent);
                let mut options = options.clone();
                options.reasoning_effort = Some(reasoning_effort);
                let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
                    state.agent.supervisor.clone(),
                    root_config.clone(),
                    agent.tool_registry().clone(),
                )
                .with_background_runs(state.agent.background_runs.clone());
                let plan_tools = plan_mode.then(|| {
                    sigil_runtime::build_plan_prompt_tool_registry(
                        agent.tool_registry(),
                        root_config,
                    )
                    .into_registry()
                });
                let task_result_tx = state.run.result_tx.clone();
                let run_id = state.run.next_id;
                state.run.next_id += 1;
                let provider_logical_run_id = format!("foreground-run-{run_id}");
                let context_resolver = context_resolver.clone();
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

                let url_capability_registrar = run_session.user_url_capability_registrar();
                let image_attachment_resolver = run_session.image_attachment_resolver();
                let handle = runtime.spawn(async move {
                    let _run_task_guard = run_task_guard;
                    let mut run_session = run_session;
                    let result = {
                        let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
                        let input = chat_agent_run_input_with_repo_context(
                            &context_resolver,
                            prompt,
                            plan_mode,
                            Vec::new(),
                        )
                        .await
                        .with_image_attachments(attachments)
                        .with_logical_run_id(provider_logical_run_id.clone())
                        .with_cancellation(cancellation_handle);
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
                            provider_logical_run_id: Some(provider_logical_run_id),
                            agent_result_continuation_thread_ids: Vec::new(),
                        },
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
            RunPlanCommand::InvokeInlineSkill {
                skill_id,
                arguments,
                reasoning_effort,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
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
                if loaded.descriptor.run_as != SkillRunMode::Inline {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                        "agent {skill_id} is configured for {} mode, not inline skill mode",
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

                let prompt = skill_invocation_prompt(&skill_id, &arguments);
                let _ = message_tx.send(WorkerMessage::SkillRunStarted {
                    skill_id: skill_id.clone(),
                    prompt: sigil_kernel::safe_persistence_text(&prompt),
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
                let agent = Arc::clone(agent);
                let mut options = options.clone();
                options.reasoning_effort = Some(reasoning_effort);
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

                let url_capability_registrar = run_session.user_url_capability_registrar();
                let image_attachment_resolver = run_session.image_attachment_resolver();
                let handle = runtime.spawn(async move {
                    let _run_task_guard = run_task_guard;
                    let mut run_session = run_session;
                    let input = AgentRunInput::transient(prompt, vec![loaded.transient_context])
                        .with_cancellation(cancellation_handle);
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
                            provider_logical_run_id: None,
                            agent_result_continuation_thread_ids: Vec::new(),
                        },
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
            RunPlanCommand::ApprovalDecision { call_id, approved } => {
                if let Some(active_run) = &state.run.active {
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
            RunPlanCommand::ApprovalDecisionWithArgs { call_id, args_json } => {
                if let Some(active_run) = &state.run.active {
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
            RunPlanCommand::ApprovalSessionDecision { call_id } => {
                if let Some(active_run) = &state.run.active {
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
            RunPlanCommand::ApprovalCommand(command) => {
                if state
                    .processed_worker_command_ids
                    .contains(&command.command_id)
                {
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
                if let Some(active_run) = &state.run.active {
                    if active_run.approval_tx.send(signal).is_ok() {
                        state
                            .processed_worker_command_ids
                            .insert(command.command_id);
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
            RunPlanCommand::CancelRun => {
                if let Some(active_run) = state.run.active.take() {
                    cancel_active_run(
                        active_run,
                        runtime,
                        root_config,
                        &state.session.log_path,
                        &mut state.session.current,
                        message_tx,
                        elicitation_handler,
                        &state.agent.supervisor,
                        &mut state.run.discarded_ids,
                        "run cancelled from TUI",
                    );
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "no active run to cancel".to_owned(),
                    ));
                }
            }
            RunPlanCommand::RejectPlan {
                plan_id,
                expected_plan_hash,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before rejecting a plan".to_owned(),
                    ));
                    continue;
                }
                match reject_plan(
                    root_config,
                    &state.session.log_path,
                    &mut state.session.current,
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
            RunPlanCommand::ApprovePlan {
                plan_text,
                permission,
                scope_summary,
                clear_planning_context,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before approving a plan".to_owned(),
                    ));
                    continue;
                }
                match approve_plan(
                    root_config,
                    &state.session.log_path,
                    &mut state.session.current,
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
        }
    }
    control
}
