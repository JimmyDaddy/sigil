use super::super::agent_runtime::{
    PlanReviewExecutionResult, apply_user_input_decision, chat_agent_run_input_with_repo_context,
    effective_orchestration_root_config, run_automatic_plan_review, run_explicit_plan_review,
    run_prepared_plan_review,
};
use super::*;
use sigil_kernel::EventHandler;

fn record_tui_revision_spawn_failure(
    session: &mut sigil_kernel::Session,
    request: &sigil_runtime::PlanReviewRunRequest,
    reason: &str,
) -> anyhow::Result<()> {
    let (Some(base_plan_id), Some(base_plan_hash)) = (
        request.base_plan_id.as_ref(),
        request.base_plan_hash.as_ref(),
    ) else {
        anyhow::bail!("revision request is missing its base plan binding");
    };
    sigil_runtime::PlanReviewCoordinator::record_revision_failure(
        session,
        base_plan_id,
        base_plan_hash,
        reason,
        current_unix_time_ms(),
    )?;
    Ok(())
}

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
        permission_mode_override,
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
                ..
            } => {
                // A new run starts with the persisted permission mode; a runtime switch made
                // during the previous run must not leak into it.
                permission_mode_override.clear();
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

                if let Some(current) = state.session.current.as_ref() {
                    match sigil_runtime::application_run::application_session_has_unresolved_user_input(
                        &state.session.log_path,
                        current.session_scope_id(),
                    ) {
                        Ok(true) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(
                                "answer, decline, or cancel the pending input before starting another turn"
                                    .to_owned(),
                            ));
                            continue;
                        }
                        Ok(false) => {}
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                                "failed to verify the pending input frontier: {error:#}"
                            )));
                            continue;
                        }
                    }
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
                    Some(sigil_runtime::application_run::ApplicationPostRunMaintenance::session_title(
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
                    sigil_runtime::build_plan_review_tool_registry(
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
                .with_writable_memory_routing(root_config.memory.writable)
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
                        if plan_mode {
                            let plan_registry = plan_tools
                                .expect("plan mode must construct its scoped review registry");
                            let result = run_explicit_plan_review(
                                &mut run_session,
                                &prompt,
                                &provider_logical_run_id,
                                sigil_runtime::plan_handoff_workspace_snapshot_id(
                                    plan_review_root_config.as_ref(),
                                    &options.workspace_root,
                                )
                                .ok()
                                .flatten(),
                                agent.as_ref(),
                                options.clone(),
                                plan_registry,
                                &mut handler,
                                &mut approval_handler,
                                cancellation_handle.clone(),
                            )
                            .await;
                            match result {
                                Ok(PlanReviewExecutionResult::Finished(result)) => {
                                    RunTaskPayload::Chat {
                                        result: Ok(result),
                                        plan_mode: true,
                                        plan_review: true,
                                        queue_id: None,
                                        provider_logical_run_id: Some(
                                            provider_logical_run_id.clone(),
                                        ),
                                        agent_result_continuation_thread_ids: Vec::new(),
                                    }
                                }
                                Ok(PlanReviewExecutionResult::AwaitingUserInput(request)) => {
                                    RunTaskPayload::AwaitingUserInput { request }
                                }
                                Err(error) => RunTaskPayload::Chat {
                                    result: Err(error),
                                    plan_mode: true,
                                    plan_review: true,
                                    queue_id: None,
                                    provider_logical_run_id: Some(provider_logical_run_id.clone()),
                                    agent_result_continuation_thread_ids: Vec::new(),
                                },
                            }
                        } else {
                            let input = chat_agent_run_input_with_repo_context(
                                &context_resolver,
                                prompt,
                                false,
                                Vec::new(),
                            )
                            .await
                            .with_image_attachments(attachments)
                            .with_tool_artifact_read_budget(tool_artifact_read_budget.clone());
                            let input = conversation_coordinator
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
                                .map_err(|error| format!("{error:#}"));
                            let output = match input {
                                Ok(input) => {
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
                                AgentRunDisposition::AwaitingUserInput(request) => {
                                    RunTaskPayload::AwaitingUserInput { request }
                                }
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
                                AgentRunDisposition::ContinueDurableTask(action) => {
                                    let task_id = action.task_id.as_str().to_owned();
                                    let task = sigil_runtime::validate_task_continuation_action(
                                        &run_session,
                                        &action,
                                    )
                                    .map_err(|error| {
                                        format!("typed task continuation is stale: {error}")
                                    });
                                    match task {
                                        Ok(task) => {
                                            let _ = run_message_tx.send(
                                                WorkerMessage::TaskRunStarted {
                                                    task_id: task_id.clone(),
                                                    objective: task.objective.clone(),
                                                },
                                            );
                                            let result =
                                                continue_routed_task_to_root_terminal(
                                                    &mut run_session,
                                                    RoutedTaskContinuationOrchestration {
                                                        task_id: action.task_id,
                                                        parent_session_ref:
                                                            task.parent_session_ref,
                                                        objective: task.objective,
                                                        guidance: action.guidance,
                                                        guidance_receipt:
                                                            action.guidance_receipt,
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
                                        Err(error) => {
                                            let error = if cancellation_handle
                                                .try_finalize_naturally()
                                            {
                                                error
                                            } else {
                                                "run cancellation won the stale-task terminal-state race"
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
                                    match result {
                                        Ok(PlanReviewExecutionResult::Finished(result)) => {
                                            RunTaskPayload::Chat {
                                                result: Ok(result),
                                                plan_mode,
                                                plan_review: true,
                                                queue_id: None,
                                                provider_logical_run_id: Some(
                                                    provider_logical_run_id.clone(),
                                                ),
                                                agent_result_continuation_thread_ids: Vec::new(),
                                            }
                                        }
                                        Ok(PlanReviewExecutionResult::AwaitingUserInput(
                                            request,
                                        )) => RunTaskPayload::AwaitingUserInput { request },
                                        Err(error) => RunTaskPayload::Chat {
                                            result: Err(error),
                                            plan_mode,
                                            plan_review: true,
                                            queue_id: None,
                                            provider_logical_run_id: Some(
                                                provider_logical_run_id.clone(),
                                            ),
                                            agent_result_continuation_thread_ids: Vec::new(),
                                        },
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
                                    provider_logical_run_id: Some(
                                        provider_logical_run_id.clone(),
                                    ),
                                    agent_result_continuation_thread_ids: Vec::new(),
                                },
                            }
                        }
                    };
                    if let Err(error) = append_mcp_elicitation_audits(
                        &mut run_session,
                        &run_elicitation_audit_buffer,
                    ) {
                        payload = match payload {
                            RunTaskPayload::AwaitingUserInput { .. } => RunTaskPayload::Chat {
                                result: Err(error),
                                plan_mode,
                                plan_review: false,
                                queue_id: None,
                                provider_logical_run_id: Some(provider_logical_run_id.clone()),
                                agent_result_continuation_thread_ids: Vec::new(),
                            },
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
                    let _ = task_result_tx.send(RunTaskResult {
                        run_id,
                        session: run_session,
                        payload,
                        post_run_maintenance: pending_session_title,
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
                        post_run_maintenance: None,
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
            RunPlanCommand::SubmitUserInputDecision {
                command_id,
                request_id,
                generation,
                expected_request_hash,
                decision,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before answering user input".to_owned(),
                    ));
                    continue;
                }
                let exact = state.session.current.as_ref().and_then(|session| {
                    session
                        .user_input_projection()
                        .ok()
                        .and_then(|projection| {
                            projection.public_requests().into_iter().find(|request| {
                                request.identity.request_id.as_str() == request_id
                                    && request.identity.generation == generation
                                    && request.request_hash == expected_request_hash
                            })
                        })
                        .or_else(|| {
                            let request_id =
                                sigil_kernel::UserInputRequestId::new(request_id.to_owned())
                                    .ok()?;
                            sigil_kernel::PlanReviewProjection::from_entries(session.entries())
                                .attempt_for_pending_user_input_key(
                                    &request_id,
                                    generation,
                                    &expected_request_hash,
                                )
                                .and_then(|attempt| attempt.pending_user_input.as_deref().cloned())
                        })
                        .or_else(|| {
                            sigil_kernel::AgentUserInputRouteProjectionV1::from_session_entries(
                                session.entries(),
                            )
                            .ok()?
                            .unresolved()
                            .find(|route| {
                                route.request.identity.request_id.as_str() == request_id
                                    && route.request.identity.generation == generation
                                    && route.request.request_hash == expected_request_hash
                            })
                            .map(|route| route.request.clone())
                        })
                });
                let Some(exact) = exact else {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "user input request is stale; reload the session".to_owned(),
                    ));
                    continue;
                };
                let tool_artifact_read_budget =
                    state.session.begin_root_tool_artifact_read_budget();
                let command_binding = format!(
                    "{}\0{}\0{}\0{}\0{}\0{}",
                    exact.identity.session_scope_id.as_str(),
                    exact.identity.root_logical_run_id.as_str(),
                    exact.identity.source_thread_id.as_str(),
                    exact.identity.request_id.as_str(),
                    exact.identity.generation,
                    exact.request_hash,
                );
                let command_id =
                    match sigil_kernel::UserInputCommandId::new(command_id.unwrap_or_else(|| {
                        format!(
                            "tui-input-{}",
                            sigil_kernel::stable_event_hash(command_binding)
                        )
                    })) {
                        Ok(command_id) => command_id,
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                                "user input command identity failed: {error:#}"
                            )));
                            continue;
                        }
                    };
                let child_route = state.session.current.as_ref().and_then(|session| {
                    sigil_kernel::AgentUserInputRouteProjectionV1::from_session_entries(
                        session.entries(),
                    )
                    .ok()
                    .and_then(|projection| {
                        projection
                            .route_for_request(&exact.identity, &exact.request_hash)
                            .cloned()
                    })
                    .filter(|route| {
                        matches!(
                            route.status,
                            sigil_kernel::AgentRouteStatus::Requested
                                | sigil_kernel::AgentRouteStatus::Registered
                        )
                    })
                });
                if let Some(route) = child_route.as_ref().filter(|route| {
                    matches!(
                        route.request.source,
                        sigil_kernel::UserInputSourceV1::Planner { .. }
                    )
                }) {
                    let command = sigil_kernel::UserInputDecisionCommandV1 {
                        identity: exact.identity,
                        request_hash: exact.request_hash,
                        command_id,
                        decision,
                    };
                    let mut handler = ChannelEventHandler::new(message_tx.clone());
                    if !matches!(
                        command.decision,
                        sigil_kernel::UserInputDecisionV1::Submitted { .. }
                    ) {
                        let result = state.session.current.as_mut().map_or_else(
                            || Err(anyhow::anyhow!("session state is unavailable for task planner input")),
                            |session| {
                                sigil_runtime::agent_supervisor::task_role_runtime::settle_task_planner_user_input_without_continuation(
                                    session,
                                    route,
                                    command,
                                )
                            },
                        );
                        match result {
                            Ok((receipt, controls)) => {
                                for control in controls {
                                    if let Err(error) = handler.handle(RunEvent::Control(control)) {
                                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                                            "task planner input event delivery failed: {error:#}"
                                        )));
                                    }
                                }
                                let entries = state
                                    .session
                                    .current
                                    .as_ref()
                                    .map(|session| session.entries().to_vec())
                                    .unwrap_or_default();
                                let _ = message_tx.send(WorkerMessage::UserInputDecisionApplied {
                                    request: receipt.request,
                                    continuation_started: false,
                                    entries,
                                });
                            }
                            Err(error) => {
                                let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                                    "task planner input decision failed: {error:#}"
                                )));
                            }
                        }
                        continue;
                    }
                    let Some(mut run_session) = state.session.current.take() else {
                        let _ = message_tx.send(WorkerMessage::RunFailed(
                            "session state is unavailable for task planner input".to_owned(),
                        ));
                        continue;
                    };
                    let task = run_session
                        .task_state_projection()
                        .tasks
                        .get(&route.budget_scope_id)
                        .cloned();
                    let Some(task) = task else {
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(
                            "task planner input references an unavailable task".to_owned(),
                        ));
                        continue;
                    };
                    let task_id = task.task_id.clone();
                    let task_id_value = task_id.as_str().to_owned();
                    let effective_config =
                        effective_orchestration_root_config(root_config, &run_session);
                    let (approval_tx, approval_rx) = mpsc::channel();
                    let elicitation_audit_buffer: McpElicitationAuditBuffer =
                        Arc::new(std::sync::Mutex::new(Vec::new()));
                    elicitation_handler
                        .set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                    let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                    let run_id = state.allocate_run_id();
                    let (
                        cancellation_owner,
                        cancellation_recorder,
                        cancellation_handle,
                        cancellation_task_guard,
                    ) = match prepare_task_run_cancellation(&mut run_session, &task_id) {
                        Ok(cancellation) => cancellation,
                        Err(error) => {
                            state.session.current = Some(run_session);
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                            continue;
                        }
                    };
                    if let Err(error) = state
                        .acquire_route_execution_owner_for_scope(run_session.session_scope_id())
                    {
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                    let prepared = runtime.block_on(
                        sigil_runtime::agent_supervisor::task_role_runtime::prepare_task_planner_user_input_continuation(
                            &effective_config,
                            options,
                            agent.tool_registry(),
                            state.agent.supervisor.clone(),
                            role_provider_builder.as_ref(),
                            &mut run_session,
                            route,
                            &command,
                        ),
                    );
                    let prepared = match prepared {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            state.run.route_execution_owner = None;
                            state.session.current = Some(run_session);
                            let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                                "task planner continuation preparation failed: {error:#}"
                            )));
                            continue;
                        }
                    };
                    let entries = run_session.entries().to_vec();
                    let _ = message_tx.send(WorkerMessage::UserInputDecisionApplied {
                        request: prepared.receipt.request.clone(),
                        continuation_started: true,
                        entries,
                    });
                    let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                        task_id: task_id_value.clone(),
                        objective: task.objective.clone(),
                    });
                    let url_capability_registrar = run_session.user_url_capability_registrar();
                    let image_attachment_resolver = run_session.image_attachment_resolver();
                    let cancellation_target = RunCancellationTarget::Task {
                        task_id: task_id_value.clone(),
                    };
                    let handle = spawn_task_planner_input(
                        runtime,
                        TaskPlannerInputSpawn {
                            run_id,
                            session: run_session,
                            task_id,
                            task_id_value,
                            parent_session_ref: task.parent_session_ref,
                            objective: task.objective,
                            route: prepared.route,
                            command,
                            task_runtime: prepared.runtime,
                            max_plan_steps: effective_config.task.max_plan_steps,
                            task_result_tx: state.run.result_tx.clone(),
                            approval_rx,
                            handler,
                            elicitation_audit_buffer: run_elicitation_audit_buffer,
                            cancellation_handle,
                            cancellation_task_guard,
                            tool_artifact_read_budget,
                        },
                    );
                    state.run.active = Some(ActiveRun {
                        run_id,
                        handle,
                        approval_tx,
                        elicitation_audit_buffer,
                        cancellation_owner,
                        cancellation_recorder,
                        cancellation_target,
                        url_capability_registrar,
                        image_attachment_resolver,
                    });
                    continue;
                }
                if child_route.is_some() {
                    let effective_config = state
                        .session
                        .current
                        .as_ref()
                        .map(|session| effective_orchestration_root_config(root_config, session))
                        .unwrap_or_else(|| root_config.clone());
                    let command = sigil_kernel::UserInputDecisionCommandV1 {
                        identity: exact.identity,
                        request_hash: exact.request_hash,
                        command_id,
                        decision,
                    };
                    let mut delegate = sigil_runtime::AgentToolRuntime::new(
                        state.agent.supervisor.clone(),
                        effective_config,
                        agent.tool_registry().clone(),
                    )
                    .with_background_runs(state.agent.background_runs.clone());
                    let mut handler = ChannelEventHandler::new(message_tx.clone());
                    let result = {
                        let Some(session) = state.session.current.as_mut() else {
                            let _ = message_tx.send(WorkerMessage::RunFailed(
                                "session state is unavailable for background child input"
                                    .to_owned(),
                            ));
                            continue;
                        };
                        runtime.block_on(delegate.apply_background_user_input_decision(
                            session,
                            command,
                            options,
                            &mut handler,
                        ))
                    };
                    match result {
                        Ok(result) => {
                            let entries = state
                                .session
                                .current
                                .as_ref()
                                .map(|session| session.entries().to_vec())
                                .unwrap_or_default();
                            let _ = message_tx.send(WorkerMessage::UserInputDecisionApplied {
                                request: result.receipt.request,
                                continuation_started: result.continuation_started,
                                entries,
                            });
                        }
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                                "background child input decision failed: {error:#}"
                            )));
                        }
                    }
                    continue;
                }
                if matches!(
                    &exact.source,
                    sigil_kernel::UserInputSourceV1::PlanRevision { .. }
                        | sigil_kernel::UserInputSourceV1::PlanReviewResearch { .. }
                ) {
                    let accepted = {
                        let Some(session) = state.session.current.as_mut() else {
                            let _ = message_tx.send(WorkerMessage::RunFailed(
                                "session state is unavailable for plan revision guidance"
                                    .to_owned(),
                            ));
                            continue;
                        };
                        let revision_guidance = matches!(
                            &exact.source,
                            sigil_kernel::UserInputSourceV1::PlanRevision { .. }
                        );
                        let command = sigil_kernel::UserInputDecisionCommandV1 {
                            identity: exact.identity,
                            request_hash: exact.request_hash,
                            command_id,
                            decision,
                        };
                        if revision_guidance {
                            let snapshot_id =
                                match sigil_runtime::plan_handoff_workspace_snapshot_id(
                                    root_config,
                                    workspace_root,
                                ) {
                                    Ok(snapshot_id) => snapshot_id,
                                    Err(error) => {
                                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                                            "failed to capture revision workspace snapshot: {error}"
                                        )));
                                        continue;
                                    }
                                };
                            sigil_runtime::PlanReviewCoordinator::accept_plan_revision_guidance(
                                session,
                                command,
                                snapshot_id,
                                current_unix_time_ms(),
                            )
                        } else {
                            sigil_runtime::PlanReviewCoordinator::accept_plan_review_research_input(
                                session,
                                command,
                                current_unix_time_ms(),
                            )
                        }
                    };
                    let (receipt, revision_request) = match accepted {
                        Ok(accepted) => accepted,
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                                "plan review input decision failed: {error:#}"
                            )));
                            continue;
                        }
                    };
                    let entries = state
                        .session
                        .current
                        .as_ref()
                        .map(|session| session.entries().to_vec())
                        .unwrap_or_default();
                    let _ = message_tx.send(WorkerMessage::UserInputDecisionApplied {
                        request: receipt.request,
                        continuation_started: revision_request.is_some(),
                        entries,
                    });
                    let Some(run_request) = revision_request else {
                        continue;
                    };
                    let Some(mut run_session) = state.session.current.take() else {
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
                    let run_id = state.allocate_run_id();
                    let cancellation_recorder = match run_session.run_cancellation_recorder() {
                        Ok(recorder) => recorder,
                        Err(error) => {
                            let recovery = record_tui_revision_spawn_failure(
                                &mut run_session,
                                &run_request,
                                &format!("revision cancellation setup failed: {error}"),
                            );
                            state.session.current = Some(run_session);
                            let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                                "failed to create cancellation recorder for plan revision: {error}{}",
                                recovery
                                    .err()
                                    .map_or_else(String::new, |recovery_error| format!(
                                        "; recovery failed: {recovery_error:#}"
                                    ))
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
                    if let Err(error) = state
                        .acquire_route_execution_owner_for_scope(run_session.session_scope_id())
                    {
                        let recovery = record_tui_revision_spawn_failure(
                            &mut run_session,
                            &run_request,
                            &format!("revision route ownership failed: {error}"),
                        );
                        state.session.current = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                            "{error}{}",
                            recovery
                                .err()
                                .map_or_else(String::new, |recovery_error| format!(
                                    "; recovery failed: {recovery_error:#}"
                                ))
                        )));
                        continue;
                    }
                    let (approval_tx, approval_rx) = mpsc::channel();
                    let elicitation_audit_buffer: McpElicitationAuditBuffer =
                        Arc::new(std::sync::Mutex::new(Vec::new()));
                    elicitation_handler
                        .set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                    let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                    let task_result_tx = state.run.result_tx.clone();
                    let handle = runtime.spawn(async move {
                        let _run_task_guard = run_task_guard;
                        let mut run_session = run_session;
                        let _ = run_message_tx.send(WorkerMessage::PlanRunStarted {
                            prompt: format!(
                                "plan revision {}",
                                run_request.plan_review_id.as_str()
                            ),
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
                        let payload = match result {
                            Ok(PlanReviewExecutionResult::Finished(result)) => {
                                RunTaskPayload::Chat {
                                    result: Ok(result),
                                    plan_mode: false,
                                    plan_review: true,
                                    queue_id: None,
                                    provider_logical_run_id: None,
                                    agent_result_continuation_thread_ids: Vec::new(),
                                }
                            }
                            Ok(PlanReviewExecutionResult::AwaitingUserInput(request)) => {
                                RunTaskPayload::AwaitingUserInput { request }
                            }
                            Err(error) => RunTaskPayload::Chat {
                                result: Err(error),
                                plan_mode: false,
                                plan_review: true,
                                queue_id: None,
                                provider_logical_run_id: None,
                                agent_result_continuation_thread_ids: Vec::new(),
                            },
                        };
                        let _ = task_result_tx.send(RunTaskResult {
                            run_id,
                            session: run_session,
                            payload,
                            post_run_maintenance: None,
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
                    continue;
                }
                match apply_user_input_decision(
                    runtime,
                    Arc::clone(agent),
                    &state.agent.supervisor,
                    root_config,
                    agent.tool_registry(),
                    options,
                    &state.agent.background_runs,
                    &mut state.session.current,
                    &state.run.result_tx,
                    message_tx,
                    Arc::clone(elicitation_handler),
                    &mut state.run.next_id,
                    tool_artifact_read_budget,
                    exact.identity,
                    exact.request_hash,
                    command_id,
                    decision,
                ) {
                    Ok(Some(active)) => state.run.active = Some(active),
                    Ok(None) => {}
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
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
                let _ = message_tx.send(WorkerMessage::UserInputRequested {
                    request: revised.request,
                    entries: revised.entries,
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
