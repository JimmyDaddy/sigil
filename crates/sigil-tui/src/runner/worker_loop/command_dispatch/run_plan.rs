use super::super::agent_runtime::{
    chat_agent_run_input_with_repo_context, effective_orchestration_root_config,
    run_automatic_plan_review, run_prepared_plan_review,
};
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
        config_path,
        provider_capabilities,
        workspace_root,
        options,
        message_tx,
        elicitation_handler,
        mcp_event_handler: _,
        role_provider_builder,
        context_resolver,
        state,
    } = context;

    let plan_review_root_config = Arc::new(root_config.clone());
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
                    match queue_conversation_input_and_track_detached(
                        &state.session.log_path,
                        &mut state.session.current,
                        &mut state.session.detached_durable_controls,
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
                    state.session.task_guidance_dirty = true;
                    state.session.conversation_queue_dirty = true;
                    continue;
                }

                let Some(run_session) = state.session.current.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                let tool_artifact_read_budget =
                    state.session.begin_root_tool_artifact_read_budget();

                let pending_session_title = if !cfg!(test)
                    && !plan_mode
                    && !prompt.trim().is_empty()
                    && !run_session
                        .entries()
                        .iter()
                        .any(|entry| matches!(entry, SessionLogEntry::User(_)))
                    && let Some(route) = run_session.resolved_model_route().cloned()
                {
                    let title_root_config = root_config.clone();
                    let title_workspace_root = workspace_root.clone();
                    let title_session_log_path = state.session.log_path.clone();
                    let title_session_id = run_session.session_scope_id().to_owned();
                    let title_prompt = prompt.clone();
                    Some((
                        title_root_config,
                        title_workspace_root,
                        route.model_ref,
                        title_session_log_path,
                        title_session_id,
                        title_prompt,
                    ))
                } else {
                    None
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
                let run_message_tx = message_tx.clone();
                let agent = Arc::clone(agent);
                let mut options = options.clone();
                options.reasoning_effort = sigil_runtime::admitted_reasoning_effort(
                    run_session.provider_name(),
                    run_session.model_name(),
                    Some(reasoning_effort),
                );
                let effective_root_config =
                    effective_orchestration_root_config(root_config, &run_session);
                let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
                    state.agent.supervisor.clone(),
                    effective_root_config.clone(),
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
                let run_id = state.allocate_run_id();
                let provider_logical_run_id = format!("foreground-run-{run_id}");
                let parent_session_ref = match session_ref_for_log_path(&state.session.log_path) {
                    Ok(session_ref) => session_ref,
                    Err(error) => {
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let conversation_coordinator = ConversationCoordinator::new(
                    root_config.task.enabled && !plan_mode,
                    root_config.task.routing_policy,
                )
                .with_orchestration_route_guard(sigil_runtime::OrchestrationRouteGuard::new(
                    &root_config.agent.runtime_provider,
                    &root_config.agent.model,
                    sigil_runtime::ORCHESTRATION_RUNTIME_BUILD_ID,
                ))
                .with_route_capability_evidence(
                    sigil_runtime::RouteCapabilityEvidence {
                        provider_supports_routing_tools: provider_capabilities.supports_tool_stream,
                        route_qualified: sigil_runtime::route_qualification_evidence(root_config),
                    },
                );
                let task_root_config = effective_root_config;
                let task_base_registry = agent.tool_registry().clone();
                let task_agent_supervisor = state.agent.supervisor.clone();
                let task_role_provider_builder = Arc::clone(role_provider_builder);
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
                if let Err(error) =
                    state.acquire_route_execution_owner_for_scope(run_session.session_scope_id())
                {
                    state.session.current = Some(run_session);
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    continue;
                }
                let plan_review_root_config = Arc::clone(&plan_review_root_config);
                let handle = runtime.spawn(async move {
                    let _run_task_guard = run_task_guard;
                    let mut run_session = run_session;
                    let mut payload = {
                        let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
                        let input = chat_agent_run_input_with_repo_context(
                            &context_resolver,
                            prompt,
                            plan_mode,
                            Vec::new(),
                        )
                        .await
                        .with_image_attachments(attachments)
                        .with_tool_artifact_read_budget(tool_artifact_read_budget.clone());
                        let input = if plan_mode {
                            // Plan-mode prompts are intentionally transient and therefore have no
                            // durable user turn for ConversationCoordinator to bind. They keep the
                            // ordinary logical-run/cancellation contract but cannot request an
                            // automatic conversation-to-task handoff.
                            Ok(input
                                .with_logical_run_id(provider_logical_run_id.clone())
                                .with_cancellation(cancellation_handle.clone()))
                        } else {
                            conversation_coordinator
                                .enforce_orchestration_route_kill_switch(
                                    &mut run_session,
                                    current_unix_time_ms(),
                                )
                                .and_then(|_| {
                                    conversation_coordinator.bind_conversation_input(
                                        &run_session,
                                        input,
                                        parent_session_ref.clone(),
                                        provider_logical_run_id.clone(),
                                        None,
                                        current_unix_time_ms(),
                                    )
                                })
                                .map(|input| input.with_cancellation(cancellation_handle.clone()))
                                .map_err(|error| format!("{error:#}"))
                        };
                        let output = match input {
                            Ok(input) => if let Some(tools) = plan_tools {
                                agent
                                    .run_with_approval_input_tool_registry_and_agent_delegate(
                                        &mut run_session,
                                        input,
                                        options.clone(),
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
                                        options.clone(),
                                        &mut handler,
                                        &mut approval_handler,
                                        &mut agent_delegate,
                                    )
                                    .await
                            }
                            .map_err(|error| format!("{error:#}")),
                            Err(error) => Err(error),
                        };
                        match output {
                            Ok(output) => match output.disposition {
                                AgentRunDisposition::FinalAnswer => RunTaskPayload::Chat {
                                    result: Ok(output.result),
                                    plan_mode,
                                    plan_review: false,
                                    queue_id: None,
                                    provider_logical_run_id: Some(provider_logical_run_id.clone()),
                                    agent_result_continuation_thread_ids: Vec::new(),
                                },
                                AgentRunDisposition::StartDurableTask(action) => {
                                    let projection = run_session.task_state_projection();
                                    let task = projection.tasks.get(&action.task_id).cloned();
                                    match task {
                                        Some(task) => {
                                            let task_id = action.task_id.as_str().to_owned();
                                            let _ = run_message_tx.send(
                                                WorkerMessage::TaskRunStarted {
                                                    task_id: task_id.clone(),
                                                    objective: task.objective.clone(),
                                                },
                                            );
                                            let result = run_admitted_task_to_root_terminal(
                                                &mut run_session,
                                                AdmittedTaskRunOrchestration {
                                                    task_id: action.task_id,
                                                    parent_session_ref: task.parent_session_ref,
                                                    objective: task.objective,
                                                    root_config: task_root_config,
                                                    options,
                                                    base_registry: task_base_registry,
                                                    agent_supervisor: task_agent_supervisor,
                                                    role_provider_builder:
                                                        task_role_provider_builder.as_ref(),
                                                    handler: &mut handler,
                                                    cancellation_handle,
                                                    tool_artifact_read_budget,
                                                },
                                                &mut approval_handler,
                                            )
                                            .await;
                                            RunTaskPayload::Task {
                                                task_id,
                                                queue_id: None,
                                                result,
                                            }
                                        }
                                        None => {
                                            let error = if cancellation_handle
                                                .try_finalize_naturally()
                                            {
                                                "accepted task handoff is missing its durable task"
                                                    .to_owned()
                                            } else {
                                                "run cancellation won the missing-task terminal-state race"
                                                    .to_owned()
                                            };
                                            RunTaskPayload::Chat {
                                                result: Err(error),
                                                plan_mode,
                                                plan_review: false,
                                                queue_id: None,
                                                provider_logical_run_id: None,
                                                agent_result_continuation_thread_ids: Vec::new(),
                                            }
                                        }
                                    }
                                }
                                AgentRunDisposition::Interrupted => RunTaskPayload::Chat {
                                    result: Err(
                                        "run was interrupted before a final answer".to_owned()
                                    ),
                                    plan_mode,
                                    plan_review: false,
                                    queue_id: None,
                                    provider_logical_run_id: Some(provider_logical_run_id.clone()),
                                    agent_result_continuation_thread_ids: Vec::new(),
                                },
                                AgentRunDisposition::Blocked => RunTaskPayload::Chat {
                                    result: Err("run was blocked before a final answer".to_owned()),
                                    plan_mode,
                                    plan_review: false,
                                    queue_id: None,
                                    provider_logical_run_id: Some(provider_logical_run_id.clone()),
                                    agent_result_continuation_thread_ids: Vec::new(),
                                },
                                AgentRunDisposition::TaskPlanAccepted => RunTaskPayload::Chat {
                                    result: Err(
                                        "task planning completed outside a task run".to_owned()
                                    ),
                                    plan_mode,
                                    plan_review: false,
                                    queue_id: None,
                                    provider_logical_run_id: None,
                                    agent_result_continuation_thread_ids: Vec::new(),
                                },
                                AgentRunDisposition::StartPlanReview(action) => {
                                    let _ = run_message_tx.send(WorkerMessage::PlanRunStarted {
                                        prompt: format!(
                                            "plan review {}",
                                            action.plan_review_id.as_str()
                                        ),
                                    });
                                    let plan_registry =
                                        sigil_runtime::build_plan_review_tool_registry(
                                            agent.tool_registry(),
                                            plan_review_root_config.as_ref(),
                                        )
                                        .into_registry();
                                    let result = run_automatic_plan_review(
                                        &mut run_session,
                                        action,
                                        agent.as_ref(),
                                        options.clone(),
                                        plan_registry,
                                        sigil_runtime::plan_handoff_workspace_snapshot_id(
                                            plan_review_root_config.as_ref(),
                                            &options.workspace_root,
                                        )
                                        .ok()
                                        .flatten(),
                                        &mut handler,
                                        &mut approval_handler,
                                        cancellation_handle.clone(),
                                    )
                                    .await;
                                    RunTaskPayload::Chat {
                                        result,
                                        plan_mode,
                                        plan_review: true,
                                        queue_id: None,
                                        provider_logical_run_id: Some(
                                            provider_logical_run_id.clone(),
                                        ),
                                        agent_result_continuation_thread_ids: Vec::new(),
                                    }
                                },
                                AgentRunDisposition::PlanReviewDraftSubmitted(_) => {
                                    RunTaskPayload::Chat {
                                        result: Err(
                                            "plan review draft submitted outside a plan review run"
                                                .to_owned()
                                        ),
                                        plan_mode,
                                        plan_review: false,
                                        queue_id: None,
                                        provider_logical_run_id: None,
                                        agent_result_continuation_thread_ids: Vec::new(),
                                    }
                                }
                            },
                            Err(error) => RunTaskPayload::Chat {
                                result: Err(error),
                                plan_mode,
                                plan_review: false,
                                queue_id: None,
                                provider_logical_run_id: Some(provider_logical_run_id.clone()),
                                agent_result_continuation_thread_ids: Vec::new(),
                            },
                        }
                    };
                    if let Err(error) = append_mcp_elicitation_audits(
                        &mut run_session,
                        &run_elicitation_audit_buffer,
                    ) {
                        payload = match payload {
                            RunTaskPayload::Chat {
                                plan_mode,
                                plan_review,
                                queue_id,
                                provider_logical_run_id,
                                agent_result_continuation_thread_ids,
                                ..
                            } => RunTaskPayload::Chat {
                                result: Err(error),
                                plan_mode,
                                plan_review,
                                queue_id,
                                provider_logical_run_id,
                                agent_result_continuation_thread_ids,
                            },
                            RunTaskPayload::Task {
                                task_id, queue_id, ..
                            } => RunTaskPayload::Task {
                                task_id,
                                queue_id,
                                result: Err(error),
                            },
                            RunTaskPayload::Agent { profile_id, .. } => RunTaskPayload::Agent {
                                profile_id,
                                result: Err(error),
                            },
                        };
                    }
                    if let Some((
                        title_root_config,
                        title_workspace_root,
                        title_model_ref,
                        title_session_log_path,
                        title_session_id,
                        title_prompt,
                    )) = pending_session_title
                        && let Err(error) = sigil_runtime::generate_and_persist_session_title(
                            title_root_config,
                            title_workspace_root,
                            title_model_ref,
                            title_session_log_path,
                            title_session_id,
                            title_prompt,
                        )
                        .await
                    {
                        tracing::debug!(
                            %error,
                            "semantic session title generation was not applied"
                        );
                    }
                    let _ = task_result_tx.send(RunTaskResult {
                        run_id,
                        session: run_session,
                        payload,
                    });
                });

                state.run.active = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                    cancellation_owner,
                    cancellation_recorder,
                    cancellation_target: RunCancellationTarget::Run,
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
                let tool_artifact_read_budget =
                    state.session.begin_root_tool_artifact_read_budget();

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
                let run_id = state.allocate_run_id();
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
                if let Err(error) =
                    state.acquire_route_execution_owner_for_scope(run_session.session_scope_id())
                {
                    state.session.current = Some(run_session);
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    continue;
                }
                let handle = runtime.spawn(async move {
                    let _run_task_guard = run_task_guard;
                    let mut run_session = run_session;
                    let input = AgentRunInput::transient(prompt, vec![loaded.transient_context])
                        .with_tool_artifact_read_budget(tool_artifact_read_budget)
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
                            plan_review: false,
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
                    cancellation_target: RunCancellationTarget::Run,
                    url_capability_registrar,
                    image_attachment_resolver,
                });
            }
            RunPlanCommand::ApprovalCommand(command) => {
                if let Some(receipt) = state
                    .approval_command_receipts
                    .get(&command.command_id)
                    .cloned()
                {
                    let _ = message_tx.send(WorkerMessage::ApprovalCommandReceipt(
                        WorkerApprovalCommandReceipt {
                            replayed: true,
                            ..receipt
                        },
                    ));
                    continue;
                }

                let command_id = command.command_id;
                let envelope_matches = command.protocol_version == WORKER_COMMAND_PROTOCOL_VERSION
                    && command.session_id == state.session.log_path.display().to_string();
                let (call_id, approval_request_id, decision, approval, family_pattern) =
                    match command.payload {
                        WorkerApprovalCommand::Decision {
                            call_id,
                            approval_request_id,
                            approved,
                        } => {
                            let approval = if approved {
                                ToolApproval::Approve
                            } else {
                                ToolApproval::Deny {
                                    reason: "denied in TUI".to_owned(),
                                }
                            };
                            let decision = if approved {
                                WorkerApprovalDecision::ApproveOnce
                            } else {
                                WorkerApprovalDecision::Deny
                            };
                            (call_id, approval_request_id, decision, approval, None)
                        }
                        WorkerApprovalCommand::DecisionForSession {
                            call_id,
                            approval_request_id,
                        } => (
                            call_id,
                            approval_request_id,
                            WorkerApprovalDecision::ApproveForSession,
                            ToolApproval::ApproveForSession,
                            None,
                        ),
                        WorkerApprovalCommand::DecisionForFamily {
                            call_id,
                            approval_request_id,
                            pattern,
                        } => (
                            call_id,
                            approval_request_id,
                            WorkerApprovalDecision::ApproveForFamily,
                            ToolApproval::Approve,
                            Some(pattern),
                        ),
                        WorkerApprovalCommand::DecisionWithArgs {
                            call_id,
                            approval_request_id,
                            args_json,
                        } => (
                            call_id,
                            approval_request_id,
                            WorkerApprovalDecision::ApproveWithArgs,
                            ToolApproval::ApproveWithArgs { args_json },
                            None,
                        ),
                    };

                let receipt = if !envelope_matches {
                    WorkerApprovalCommandReceipt {
                        command_id: command_id.clone(),
                        approval_request_id,
                        call_id,
                        decision,
                        route_state: WorkerApprovalRouteState::Rejected,
                        replayed: false,
                    }
                } else if let Some(active_run) = &state.run.active {
                    let (acknowledgement_tx, acknowledgement_rx) = mpsc::sync_channel(1);
                    let signal = ApprovalSignal::Decision {
                        call_id: call_id.clone(),
                        approval_request_id: approval_request_id.clone(),
                        approval,
                        acknowledgement_tx,
                    };
                    let route_state = if active_run.approval_tx.send(signal).is_err() {
                        WorkerApprovalRouteState::Rejected
                    } else {
                        match acknowledgement_rx.recv_timeout(Duration::from_secs(1)) {
                            Ok(acknowledgement)
                                if acknowledgement.accepted
                                    && acknowledgement.call_id == call_id
                                    && acknowledgement.approval_request_id
                                        == approval_request_id =>
                            {
                                WorkerApprovalRouteState::DecisionAccepted
                            }
                            Ok(_) => WorkerApprovalRouteState::Rejected,
                            Err(_) => WorkerApprovalRouteState::DeliveryUncertain,
                        }
                    };
                    WorkerApprovalCommandReceipt {
                        command_id: command_id.clone(),
                        approval_request_id,
                        call_id,
                        decision,
                        route_state,
                        replayed: false,
                    }
                } else {
                    WorkerApprovalCommandReceipt {
                        command_id: command_id.clone(),
                        approval_request_id,
                        call_id,
                        decision,
                        route_state: WorkerApprovalRouteState::Rejected,
                        replayed: false,
                    }
                };

                // A rejected route proves that the decision was not delivered. Do not consume the
                // idempotency key: the UI may safely retry the same exact command after a local
                // worker rebind. Accepted and uncertain routes may have reached the kernel, so
                // those receipts must remain replayable instead of delivering the decision twice.
                if receipt.route_state != WorkerApprovalRouteState::Rejected {
                    state.remember_approval_command_receipt(receipt.clone());
                }
                if envelope_matches && let Some(pattern) = family_pattern {
                    // The family rule is durable best-effort: the current command is already
                    // approved, and a persist failure only surfaces a notice. The rule applies to
                    // subsequent runs because the run options were assembled before this write.
                    let config_path = config_path.clone();
                    let notice_tx = message_tx.clone();
                    runtime.spawn_blocking(move || {
                        if let Err(error) =
                            sigil_runtime::command_permission::append_command_allow_pattern(
                                &config_path,
                                &pattern,
                            )
                        {
                            let _ = notice_tx.send(WorkerMessage::Notice(format!(
                                "could not persist command family rule {pattern:?}: {error}"
                            )));
                        }
                    });
                }
                let _ = message_tx.send(WorkerMessage::ApprovalCommandReceipt(receipt));
            }
            RunPlanCommand::PauseTask { request } => {
                let Some(active_run) = state.run.active.as_ref() else {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "task pause ignored: no active run".to_owned(),
                    ));
                    continue;
                };
                let entries = match JsonlSessionStore::read_entries(&state.session.log_path) {
                    Ok(entries) => entries,
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "task pause ignored: durable task state is unavailable ({error:#})"
                        )));
                        continue;
                    }
                };
                let active_scope_id = active_run.cancellation_owner.handle().scope_id().to_owned();
                if let Err(error) = validate_task_pause_request(
                    &request,
                    &active_run.cancellation_target,
                    &active_scope_id,
                    &entries,
                ) {
                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                        "task pause ignored: {error}"
                    )));
                    continue;
                }
                let mut active_run = state
                    .run
                    .active
                    .take()
                    .expect("validated active run remains owned by worker");
                if matches!(active_run.cancellation_target, RunCancellationTarget::Run) {
                    active_run.cancellation_target = RunCancellationTarget::Task {
                        task_id: request.task_id.as_str().to_owned(),
                    };
                }
                cancel_active_run(
                    active_run,
                    runtime,
                    root_config,
                    &state.session.log_path,
                    &mut state.session.current,
                    &mut state.session.detached_durable_controls,
                    message_tx,
                    elicitation_handler,
                    &state.agent.supervisor,
                    &mut state.run.discarded_ids,
                    ActiveRunStopDisposition::PauseTask,
                    "task paused from TUI",
                );
            }
            RunPlanCommand::CancelRun => {
                if let Some(active_run) = state.run.active.take() {
                    cancel_active_run(
                        active_run,
                        runtime,
                        root_config,
                        &state.session.log_path,
                        &mut state.session.current,
                        &mut state.session.detached_durable_controls,
                        message_tx,
                        elicitation_handler,
                        &state.agent.supervisor,
                        &mut state.run.discarded_ids,
                        ActiveRunStopDisposition::Cancel,
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
            RunPlanCommand::SavePlan {
                plan_id,
                expected_plan_hash,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before saving a plan".to_owned(),
                    ));
                    continue;
                }
                let Some(current_session) = state.session.current.as_mut() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable for plan save".to_owned(),
                    ));
                    continue;
                };
                match sigil_runtime::PlanReviewCoordinator::record_plan_decision(
                    current_session,
                    &sigil_runtime::PlanDecisionCommand {
                        plan_id,
                        expected_plan_hash,
                        decision: sigil_kernel::PlanDecision::SavedOnly,
                    },
                    current_unix_time_ms(),
                ) {
                    Ok(entry) => {
                        let entries = current_session.entries().to_vec();
                        let _ = message_tx.send(WorkerMessage::PlanSaved { entry, entries });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!("{error:#}")));
                    }
                }
            }
            RunPlanCommand::RevisePlan {
                plan_id,
                expected_plan_hash,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before revising a plan".to_owned(),
                    ));
                    continue;
                }
                let revised = match revise_plan(
                    root_config,
                    workspace_root,
                    &state.session.log_path,
                    &mut state.session.current,
                    RejectPlanRequest {
                        plan_id,
                        expected_plan_hash,
                    },
                ) {
                    Ok(revised) => revised,
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                        continue;
                    }
                };
                let Some(run_session) = state.session.current.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable for plan revision".to_owned(),
                    ));
                    continue;
                };
                let plan_registry = sigil_runtime::build_plan_review_tool_registry(
                    agent.tool_registry(),
                    plan_review_root_config.as_ref(),
                )
                .into_registry();
                let run_options = options.clone();
                let run_agent = Arc::clone(agent);
                let run_message_tx = message_tx.clone();
                let run_request = revised.request;
                let run_id = state.allocate_run_id();
                let cancellation_recorder = match run_session.run_cancellation_recorder() {
                    Ok(recorder) => recorder,
                    Err(error) => {
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                            "failed to create cancellation recorder for plan revision: {error}"
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
                if let Err(error) =
                    state.acquire_route_execution_owner_for_scope(run_session.session_scope_id())
                {
                    state.session.current = Some(run_session);
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    continue;
                }
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let task_result_tx = state.run.result_tx.clone();
                let handle = runtime.spawn(async move {
                    let _run_task_guard = run_task_guard;
                    let mut run_session = run_session;
                    let _ = run_message_tx.send(WorkerMessage::PlanRunStarted {
                        prompt: format!("plan review {}", run_request.plan_review_id.as_str()),
                    });
                    let mut handler = ChannelEventHandler::new(run_message_tx.clone());
                    let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
                    let result = run_prepared_plan_review(
                        &mut run_session,
                        &run_request,
                        run_agent.as_ref(),
                        run_options,
                        plan_registry,
                        &mut handler,
                        &mut approval_handler,
                        cancellation_handle,
                    )
                    .await;
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
                            plan_review: true,
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
                    cancellation_target: RunCancellationTarget::Run,
                    url_capability_registrar,
                    image_attachment_resolver,
                });
            }
        }
    }
    control
}

pub(in crate::runner) fn validate_task_pause_request(
    request: &sigil_kernel::TaskPauseRequest,
    cancellation_target: &RunCancellationTarget,
    active_scope_id: &str,
    entries: &[SessionLogEntry],
) -> std::result::Result<(), String> {
    sigil_runtime::agent_supervisor::task_execution::validate_task_pause_request(
        request,
        cancellation_target,
        active_scope_id,
        entries,
    )
    .map_err(|error| error.to_string())
}
