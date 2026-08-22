use super::*;

pub(in crate::runner) struct TaskRunSpawn {
    pub(in crate::runner) run_id: u64,
    pub(in crate::runner) session: Session,
    pub(in crate::runner) task_id: TaskId,
    pub(in crate::runner) task_id_value: String,
    pub(in crate::runner) parent_session_ref: SessionRef,
    pub(in crate::runner) objective: String,
    pub(in crate::runner) root_config: RootConfig,
    pub(in crate::runner) options: AgentRunOptions,
    pub(in crate::runner) base_registry: ToolRegistry,
    pub(in crate::runner) agent_supervisor: sigil_runtime::AgentSupervisor,
    pub(in crate::runner) role_provider_builder: Arc<dyn TaskRoleProviderBuilder>,
    pub(in crate::runner) task_result_tx: WorkerEventPayloadSender<RunTaskResult>,
    pub(in crate::runner) approval_rx: mpsc::Receiver<ApprovalSignal>,
    pub(in crate::runner) handler: ChannelEventHandler,
    pub(in crate::runner) elicitation_audit_buffer: McpElicitationAuditBuffer,
    pub(in crate::runner) cancellation_handle: RunCancellationHandle,
    pub(in crate::runner) cancellation_task_guard: RunTaskGuard,
    pub(in crate::runner) tool_artifact_read_budget: ToolArtifactReadBudgetV1,
}

pub(in crate::runner) struct TaskContinueSpawn {
    pub(in crate::runner) run_id: u64,
    pub(in crate::runner) session: Session,
    pub(in crate::runner) task_id: TaskId,
    pub(in crate::runner) task_id_value: String,
    pub(in crate::runner) parent_session_ref: SessionRef,
    pub(in crate::runner) objective: String,
    pub(in crate::runner) guidance: Option<String>,
    pub(in crate::runner) guidance_promotion: Option<TaskGuidancePromotedEntry>,
    pub(in crate::runner) root_config: RootConfig,
    pub(in crate::runner) options: AgentRunOptions,
    pub(in crate::runner) base_registry: ToolRegistry,
    pub(in crate::runner) agent_supervisor: sigil_runtime::AgentSupervisor,
    pub(in crate::runner) role_provider_builder: Arc<dyn TaskRoleProviderBuilder>,
    pub(in crate::runner) task_result_tx: WorkerEventPayloadSender<RunTaskResult>,
    pub(in crate::runner) approval_rx: mpsc::Receiver<ApprovalSignal>,
    pub(in crate::runner) handler: ChannelEventHandler,
    pub(in crate::runner) elicitation_audit_buffer: McpElicitationAuditBuffer,
    pub(in crate::runner) cancellation_handle: RunCancellationHandle,
    pub(in crate::runner) cancellation_task_guard: RunTaskGuard,
    pub(in crate::runner) tool_artifact_read_budget: ToolArtifactReadBudgetV1,
}

pub(in crate::runner) struct TaskPlannerInputSpawn {
    pub(in crate::runner) run_id: u64,
    pub(in crate::runner) session: Session,
    pub(in crate::runner) task_id: TaskId,
    pub(in crate::runner) task_id_value: String,
    pub(in crate::runner) parent_session_ref: SessionRef,
    pub(in crate::runner) objective: String,
    pub(in crate::runner) route: sigil_kernel::AgentUserInputRouteEntryV1,
    pub(in crate::runner) command: sigil_kernel::UserInputDecisionCommandV1,
    pub(in crate::runner) task_runtime: TaskRoleRuntime,
    pub(in crate::runner) max_plan_steps: usize,
    pub(in crate::runner) task_result_tx: WorkerEventPayloadSender<RunTaskResult>,
    pub(in crate::runner) approval_rx: mpsc::Receiver<ApprovalSignal>,
    pub(in crate::runner) handler: ChannelEventHandler,
    pub(in crate::runner) elicitation_audit_buffer: McpElicitationAuditBuffer,
    pub(in crate::runner) cancellation_handle: RunCancellationHandle,
    pub(in crate::runner) cancellation_task_guard: RunTaskGuard,
    pub(in crate::runner) tool_artifact_read_budget: ToolArtifactReadBudgetV1,
}

pub(in crate::runner) struct SkillChildRunSpawn {
    pub(in crate::runner) run_id: u64,
    pub(in crate::runner) session: Session,
    pub(in crate::runner) task_id: TaskId,
    pub(in crate::runner) task_id_value: String,
    pub(in crate::runner) parent_session_ref: SessionRef,
    pub(in crate::runner) objective: String,
    pub(in crate::runner) skill_id: String,
    pub(in crate::runner) arguments: String,
    pub(in crate::runner) loaded: sigil_runtime::LoadedSkillContext,
    pub(in crate::runner) root_config: RootConfig,
    pub(in crate::runner) options: AgentRunOptions,
    pub(in crate::runner) base_registry: ToolRegistry,
    pub(in crate::runner) agent_supervisor: sigil_runtime::AgentSupervisor,
    pub(in crate::runner) role_provider_builder: Arc<dyn TaskRoleProviderBuilder>,
    pub(in crate::runner) task_result_tx: WorkerEventPayloadSender<RunTaskResult>,
    pub(in crate::runner) approval_rx: mpsc::Receiver<ApprovalSignal>,
    pub(in crate::runner) handler: ChannelEventHandler,
    pub(in crate::runner) elicitation_audit_buffer: McpElicitationAuditBuffer,
    pub(in crate::runner) cancellation_handle: RunCancellationHandle,
    pub(in crate::runner) cancellation_task_guard: RunTaskGuard,
    pub(in crate::runner) tool_artifact_read_budget: ToolArtifactReadBudgetV1,
}

pub(in crate::runner) fn spawn_task_run(
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
            role_provider_builder,
            task_result_tx,
            approval_rx,
            mut handler,
            elicitation_audit_buffer,
            cancellation_handle,
            cancellation_task_guard,
            tool_artifact_read_budget,
        } = spawn;
        let _cancellation_task_guard = cancellation_task_guard;
        let terminal_cancellation = cancellation_handle.clone();
        let terminal_task_id = task_id.clone();
        let terminal_parent_session_ref = parent_session_ref.clone();
        let terminal_objective = objective.clone();
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
                role_provider_builder: role_provider_builder.as_ref(),
                approval_rx,
                handler: &mut handler,
                cancellation_handle,
                tool_artifact_read_budget,
            },
        )
        .await;
        let result = finalize_task_root(
            &mut session,
            &terminal_task_id,
            &terminal_parent_session_ref,
            &terminal_objective,
            &terminal_cancellation,
            result,
        );
        let result = match append_mcp_elicitation_audits(&mut session, &elicitation_audit_buffer) {
            Ok(()) => result,
            Err(error) => Err(error),
        };
        send_task_result(run_id, session, task_id_value, result, task_result_tx);
    })
}

pub(in crate::runner) fn spawn_task_continue(
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
            guidance_promotion,
            root_config,
            options,
            base_registry,
            agent_supervisor,
            role_provider_builder,
            task_result_tx,
            approval_rx,
            mut handler,
            elicitation_audit_buffer,
            cancellation_handle,
            cancellation_task_guard,
            tool_artifact_read_budget,
        } = spawn;
        let _cancellation_task_guard = cancellation_task_guard;
        let terminal_cancellation = cancellation_handle.clone();
        let terminal_task_id = task_id.clone();
        let terminal_parent_session_ref = parent_session_ref.clone();
        let terminal_objective = objective.clone();
        let continuation_entry_frontier = session.entries().len();
        let result = continue_task_orchestration(
            &mut session,
            TaskContinueOrchestration {
                task_id,
                guidance,
                guidance_promotion,
                root_config,
                options,
                base_registry,
                agent_supervisor,
                role_provider_builder: role_provider_builder.as_ref(),
                approval_rx,
                handler: &mut handler,
                cancellation_handle,
                tool_artifact_read_budget,
            },
        )
        .await;
        let result = finalize_task_continuation_root(
            &mut session,
            &terminal_task_id,
            &terminal_parent_session_ref,
            &terminal_objective,
            &terminal_cancellation,
            continuation_entry_frontier,
            result,
        );
        let result = match append_mcp_elicitation_audits(&mut session, &elicitation_audit_buffer) {
            Ok(()) => result,
            Err(error) => Err(error),
        };
        send_task_result(run_id, session, task_id_value, result, task_result_tx);
    })
}

pub(in crate::runner) fn spawn_task_planner_input(
    runtime: &tokio::runtime::Runtime,
    spawn: TaskPlannerInputSpawn,
) -> tokio::task::JoinHandle<()> {
    runtime.spawn(async move {
        let TaskPlannerInputSpawn {
            run_id,
            mut session,
            task_id,
            task_id_value,
            parent_session_ref,
            objective,
            route,
            command,
            task_runtime,
            max_plan_steps,
            task_result_tx,
            approval_rx,
            mut handler,
            elicitation_audit_buffer,
            cancellation_handle,
            cancellation_task_guard,
            tool_artifact_read_budget,
        } = spawn;
        let _cancellation_task_guard = cancellation_task_guard;
        let terminal_cancellation = cancellation_handle.clone();
        let TaskRoleRuntime {
            orchestrator,
            planner_options,
            executor_options,
            subagent_read_options,
            subagent_write_options,
        } = task_runtime;
        let orchestrator = orchestrator
            .with_cancellation(cancellation_handle)
            .with_tool_artifact_read_budget(tool_artifact_read_budget);
        let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
        let result = orchestrator
            .resume_planner_after_user_input(
                &mut session,
                SequentialTaskRequest {
                    task_id: task_id.clone(),
                    parent_session_ref: parent_session_ref.clone(),
                    objective: objective.clone(),
                },
                route,
                command,
                planner_options,
                executor_options,
                subagent_read_options,
                subagent_write_options,
                max_plan_steps,
                &mut handler,
                &mut approval_handler,
            )
            .await
            .map(|output| output.status);
        let result = finalize_task_root(
            &mut session,
            &task_id,
            &parent_session_ref,
            &objective,
            &terminal_cancellation,
            result,
        );
        let result = match append_mcp_elicitation_audits(&mut session, &elicitation_audit_buffer) {
            Ok(()) => result,
            Err(error) => Err(error),
        };
        send_task_result(run_id, session, task_id_value, result, task_result_tx);
    })
}

pub(in crate::runner) fn spawn_skill_child_run(
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
            role_provider_builder,
            task_result_tx,
            approval_rx,
            mut handler,
            elicitation_audit_buffer,
            cancellation_handle,
            cancellation_task_guard,
            tool_artifact_read_budget,
        } = spawn;
        let _cancellation_task_guard = cancellation_task_guard;
        let terminal_cancellation = cancellation_handle.clone();
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
                role_provider_builder: role_provider_builder.as_ref(),
                approval_rx,
                handler: &mut handler,
                cancellation_handle,
                tool_artifact_read_budget,
            },
        )
        .await;
        let result = if terminal_cancellation.is_naturally_finalized()
            || terminal_cancellation.try_finalize_naturally()
        {
            result
        } else {
            Err("run cancellation won the task terminal-state race".to_owned())
        };
        let result = match append_mcp_elicitation_audits(&mut session, &elicitation_audit_buffer) {
            Ok(()) => result,
            Err(error) => Err(error),
        };
        send_task_result(run_id, session, task_id_value, result, task_result_tx);
    })
}

pub(in crate::runner) struct TaskRunOrchestration<'a> {
    task_id: TaskId,
    parent_session_ref: SessionRef,
    objective: String,
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    role_provider_builder: &'a dyn TaskRoleProviderBuilder,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: &'a mut ChannelEventHandler,
    cancellation_handle: RunCancellationHandle,
    tool_artifact_read_budget: ToolArtifactReadBudgetV1,
}

pub(in crate::runner) struct AdmittedTaskRunOrchestration<'a> {
    pub(in crate::runner) task_id: TaskId,
    pub(in crate::runner) parent_session_ref: SessionRef,
    pub(in crate::runner) objective: String,
    pub(in crate::runner) root_config: RootConfig,
    pub(in crate::runner) options: AgentRunOptions,
    pub(in crate::runner) base_registry: ToolRegistry,
    pub(in crate::runner) agent_supervisor: sigil_runtime::AgentSupervisor,
    pub(in crate::runner) role_provider_builder: &'a dyn TaskRoleProviderBuilder,
    pub(in crate::runner) handler: &'a mut ChannelEventHandler,
    pub(in crate::runner) cancellation_handle: RunCancellationHandle,
    pub(in crate::runner) tool_artifact_read_budget: ToolArtifactReadBudgetV1,
}

/// Runtime material for a conversation route that selected one exact existing durable Task.
///
/// This deliberately mirrors `AdmittedTaskRunOrchestration` while requiring the exact guidance
/// and durable receipt frozen by the typed route action.
pub(in crate::runner) struct RoutedTaskContinuationOrchestration<'a> {
    pub(in crate::runner) task_id: TaskId,
    pub(in crate::runner) parent_session_ref: SessionRef,
    pub(in crate::runner) objective: String,
    pub(in crate::runner) guidance: SecretString,
    pub(in crate::runner) guidance_receipt: sigil_kernel::TaskContinuationSelectedEntry,
    pub(in crate::runner) root_config: RootConfig,
    pub(in crate::runner) options: AgentRunOptions,
    pub(in crate::runner) base_registry: ToolRegistry,
    pub(in crate::runner) agent_supervisor: sigil_runtime::AgentSupervisor,
    pub(in crate::runner) role_provider_builder: &'a dyn TaskRoleProviderBuilder,
    pub(in crate::runner) handler: &'a mut ChannelEventHandler,
    pub(in crate::runner) cancellation_handle: RunCancellationHandle,
    pub(in crate::runner) tool_artifact_read_budget: ToolArtifactReadBudgetV1,
}

pub(in crate::runner) struct SkillChildRunOrchestration<'a> {
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
    role_provider_builder: &'a dyn TaskRoleProviderBuilder,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: &'a mut ChannelEventHandler,
    cancellation_handle: RunCancellationHandle,
    tool_artifact_read_budget: ToolArtifactReadBudgetV1,
}

pub(in crate::runner) struct TaskContinueOrchestration<'a> {
    task_id: TaskId,
    guidance: Option<String>,
    guidance_promotion: Option<TaskGuidancePromotedEntry>,
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    role_provider_builder: &'a dyn TaskRoleProviderBuilder,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: &'a mut ChannelEventHandler,
    cancellation_handle: RunCancellationHandle,
    tool_artifact_read_budget: ToolArtifactReadBudgetV1,
}

pub(in crate::runner) async fn run_task_orchestration(
    session: &mut Session,
    request: TaskRunOrchestration<'_>,
) -> anyhow::Result<TaskRunStatus> {
    let TaskRunOrchestration {
        task_id,
        parent_session_ref,
        objective,
        root_config,
        options,
        base_registry,
        agent_supervisor,
        role_provider_builder,
        approval_rx,
        handler,
        cancellation_handle,
        tool_artifact_read_budget,
    } = request;
    let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
    run_admitted_task_orchestration(
        session,
        AdmittedTaskRunOrchestration {
            task_id,
            parent_session_ref,
            objective,
            root_config,
            options,
            base_registry,
            agent_supervisor,
            role_provider_builder,
            handler,
            cancellation_handle,
            tool_artifact_read_budget,
        },
        &mut approval_handler,
    )
    .await
}

pub(in crate::runner) async fn run_admitted_task_orchestration<A>(
    session: &mut Session,
    request: AdmittedTaskRunOrchestration<'_>,
    approval_handler: &mut A,
) -> anyhow::Result<TaskRunStatus>
where
    A: ApprovalHandler + Send,
{
    let AdmittedTaskRunOrchestration {
        task_id,
        parent_session_ref,
        objective,
        root_config,
        options,
        base_registry,
        agent_supervisor,
        role_provider_builder,
        handler,
        cancellation_handle,
        tool_artifact_read_budget,
    } = request;
    sigil_runtime::agent_supervisor::task_execution::run_admitted_task_execution(
        session,
        sigil_runtime::agent_supervisor::task_execution::AdmittedTaskExecution {
            task_id,
            parent_session_ref,
            objective,
            root_config,
            options,
            base_registry,
            agent_supervisor,
            role_provider_builder,
            handler,
            cancellation_handle,
            tool_artifact_read_budget: Some(tool_artifact_read_budget),
        },
        approval_handler,
    )
    .await
}

/// Runs an admitted handoff task and atomically claims the shared root cancellation terminal.
///
/// Unlike ordinary `/task` spawning, conversation handoff reuses the already-open chat root. The
/// chat agent deliberately yields terminal authority when it returns `StartDurableTask`, so this
/// wrapper is the single place where direct and queued handoffs close that root after orchestration.
pub(in crate::runner) async fn run_admitted_task_to_root_terminal<A>(
    session: &mut Session,
    request: AdmittedTaskRunOrchestration<'_>,
    approval_handler: &mut A,
) -> std::result::Result<TaskRunStatus, String>
where
    A: ApprovalHandler + Send,
{
    let terminal_cancellation = request.cancellation_handle.clone();
    let terminal_task_id = request.task_id.clone();
    let terminal_parent_session_ref = request.parent_session_ref.clone();
    let terminal_objective = request.objective.clone();
    let result = run_admitted_task_orchestration(session, request, approval_handler).await;
    finalize_task_root(
        session,
        &terminal_task_id,
        &terminal_parent_session_ref,
        &terminal_objective,
        &terminal_cancellation,
        result,
    )
}

/// Continues the exact Task selected by a typed conversation route and closes the shared root.
///
/// The adapter must validate the action against the current session before calling this helper.
/// Cancellation ownership is then durably rebound to the exact Task before any role executes.
pub(in crate::runner) async fn continue_routed_task_to_root_terminal<A>(
    session: &mut Session,
    request: RoutedTaskContinuationOrchestration<'_>,
    approval_handler: &mut A,
) -> std::result::Result<TaskRunStatus, String>
where
    A: ApprovalHandler + Send,
{
    let RoutedTaskContinuationOrchestration {
        task_id,
        parent_session_ref,
        objective,
        guidance,
        guidance_receipt,
        root_config,
        options,
        base_registry,
        agent_supervisor,
        role_provider_builder,
        handler,
        cancellation_handle,
        tool_artifact_read_budget,
    } = request;
    let result = bind_task_run_cancellation_scope(session, &task_id, &cancellation_handle);
    let continuation_entry_frontier = session.entries().len();
    let result = match result {
        Ok(()) => {
            sigil_runtime::agent_supervisor::task_execution::continue_task_execution(
                session,
                sigil_runtime::agent_supervisor::task_execution::ContinuedTaskExecution {
                    requested_task_id: Some(task_id.clone()),
                    guidance: Some(guidance.expose_secret().to_owned()),
                    guidance_promotion: None,
                    continuation_guidance_receipt: Some(guidance_receipt),
                    root_config,
                    options,
                    base_registry,
                    agent_supervisor,
                    role_provider_builder,
                    handler,
                    cancellation_handle: cancellation_handle.clone(),
                    tool_artifact_read_budget: Some(tool_artifact_read_budget),
                },
                approval_handler,
            )
            .await
        }
        Err(error) => Err(anyhow::Error::msg(error)),
    };
    finalize_task_continuation_root(
        session,
        &task_id,
        &parent_session_ref,
        &objective,
        &cancellation_handle,
        continuation_entry_frontier,
        result,
    )
}

fn finalize_task_continuation_root(
    session: &mut Session,
    task_id: &TaskId,
    parent_session_ref: &SessionRef,
    objective: &str,
    terminal_cancellation: &RunCancellationHandle,
    continuation_entry_frontier: usize,
    result: anyhow::Result<TaskRunStatus>,
) -> std::result::Result<TaskRunStatus, String> {
    sigil_runtime::agent_supervisor::task_execution::finalize_task_continuation_root(
        session,
        task_id,
        parent_session_ref,
        objective,
        terminal_cancellation,
        continuation_entry_frontier,
        result,
    )
    .map_err(|error| format!("{error:#}"))
}

fn finalize_task_root(
    session: &mut Session,
    task_id: &TaskId,
    parent_session_ref: &SessionRef,
    objective: &str,
    terminal_cancellation: &RunCancellationHandle,
    result: anyhow::Result<TaskRunStatus>,
) -> std::result::Result<TaskRunStatus, String> {
    sigil_runtime::agent_supervisor::task_execution::finalize_task_root(
        session,
        task_id,
        parent_session_ref,
        objective,
        terminal_cancellation,
        result,
    )
    .map_err(|error| format!("{error:#}"))
}

pub(in crate::runner) async fn continue_task_orchestration(
    session: &mut Session,
    request: TaskContinueOrchestration<'_>,
) -> anyhow::Result<TaskRunStatus> {
    let TaskContinueOrchestration {
        task_id,
        guidance,
        guidance_promotion,
        root_config,
        options,
        base_registry,
        agent_supervisor,
        role_provider_builder,
        approval_rx,
        handler,
        cancellation_handle,
        tool_artifact_read_budget,
    } = request;
    let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
    sigil_runtime::agent_supervisor::task_execution::continue_task_execution(
        session,
        sigil_runtime::agent_supervisor::task_execution::ContinuedTaskExecution {
            requested_task_id: Some(task_id),
            guidance,
            guidance_promotion,
            continuation_guidance_receipt: None,
            root_config,
            options,
            base_registry,
            agent_supervisor,
            role_provider_builder,
            handler,
            cancellation_handle,
            tool_artifact_read_budget: Some(tool_artifact_read_budget),
        },
        &mut approval_handler,
    )
    .await
}

pub(in crate::runner) async fn run_skill_child_orchestration(
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
        role_provider_builder,
        approval_rx,
        handler,
        cancellation_handle,
        tool_artifact_read_budget,
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
        role_provider_builder,
    )
    .await?;
    let orchestrator = orchestrator
        .with_cancellation(cancellation_handle)
        .with_tool_artifact_read_budget(tool_artifact_read_budget.clone());
    session
        .append_control(ControlEntry::SkillLoaded(loaded.entry))
        .map_err(|error| format!("{error:#}"))?;
    let child_input = AgentRunInput::without_persisted_user_message(vec![
        loaded.transient_context,
        ModelMessage::user(skill_invocation_prompt(&skill_id, &arguments)),
    ])
    .with_tool_artifact_read_budget(tool_artifact_read_budget);
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
                intent_refs: Vec::new(),
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

pub(in crate::runner) fn materialize_task_verification_config(
    session: &mut Session,
    handler: &mut ChannelEventHandler,
    root_config: &RootConfig,
    workspace_root: &Path,
    task_id: &TaskId,
) -> std::result::Result<(), String> {
    sigil_runtime::agent_supervisor::task_execution::materialize_task_verification_config(
        session,
        handler,
        root_config,
        workspace_root,
        task_id,
    )
    .map_err(|error| format!("{error:#}"))
}

pub(in crate::runner) fn session_workspace_is_trusted(
    session: &Session,
    workspace_root: &Path,
) -> bool {
    let Ok(workspace_id) = stable_workspace_id(workspace_root) else {
        return false;
    };
    session
        .verification_state_projection()
        .workspace_trust
        .get(&workspace_id)
        .is_some_and(|entry| entry.trust == WorkspaceTrust::Trusted)
}

pub(in crate::runner) fn ensure_session_workspace_trust(
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
pub(in crate::runner) enum VerificationCheckPromotionKind {
    Approve,
    Sandbox,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) enum VerificationCheckPromotionOutcome {
    Promoted { entry: Box<CheckSpecRecordedEntry> },
    AlreadyPromoted { check_spec_id: String },
}

pub(in crate::runner) fn promote_workspace_verification_check(
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

pub(in crate::runner) fn promotion_matches_kind(
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

pub(in crate::runner) fn verification_check_promotion_event_id(
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

pub(in crate::runner) fn clean_mutation_artifacts(
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

pub(in crate::runner) fn delete_mutation_artifact(
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

pub(in crate::runner) fn format_mutation_artifact_cleanup_report(
    report: &MutationArtifactRetentionReport,
) -> String {
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

pub(in crate::runner) fn format_mutation_artifact_delete_report(
    payload: &MutationArtifactLifecycleRecorded,
) -> String {
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

pub(in crate::runner) async fn build_skill_child_role_runtime(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    base_registry: &ToolRegistry,
    skill: &SkillDescriptor,
    child_role: AgentRole,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    role_provider_builder: &dyn TaskRoleProviderBuilder,
) -> std::result::Result<TaskRoleRuntime, String> {
    let planner_provider = role_provider_builder
        .build(root_config, AgentRole::Planner)
        .await
        .map_err(|error| format!("{error:#}"))?;
    let executor_provider = role_provider_builder
        .build(root_config, AgentRole::Executor)
        .await
        .map_err(|error| format!("{error:#}"))?;
    let synthesis_provider = role_provider_builder
        .build(root_config, AgentRole::Planner)
        .await
        .map_err(|error| format!("{error:#}"))?;
    let subagent_read_provider = role_provider_builder
        .build(root_config, AgentRole::SubagentRead)
        .await
        .map_err(|error| format!("{error:#}"))?;
    let subagent_write_provider = role_provider_builder
        .build(root_config, AgentRole::SubagentWrite)
        .await
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
    let execution_backend = sigil_runtime::build_configured_execution_backend(root_config)
        .map_err(|error| format!("failed to build verification execution backend: {error:#}"))?;
    let child_runner = sigil_runtime::AgentSupervisorTaskChildRunner::new_with_task_roles(
        agent_supervisor,
        Agent::new(planner_provider, planner_registry),
        Agent::new(executor_provider, executor_registry),
        Agent::new(subagent_read_provider, subagent_read_registry),
        Agent::new(subagent_write_provider, subagent_write_registry),
        Agent::new(synthesis_provider, ToolRegistry::new()),
    )
    .with_provider_route_concurrency_limit(configured_provider_route_concurrency_limit(
        &root_config.task,
    ))
    .with_planner_discovery_policy(
        root_config.task.multi_agent_mode,
        root_config.task.max_planning_research_agents,
    )
    .with_integration_verification_backend(execution_backend.clone());
    Ok(TaskRoleRuntime {
        orchestrator: SequentialTaskOrchestrator::new_with_child_runner(child_runner)
            .with_max_parallel_read_steps(configured_max_parallel_read_steps(&root_config.task))
            .with_max_parallel_changeset_steps(configured_max_parallel_changeset_steps(
                &root_config.task,
            ))
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

pub(in crate::runner) fn configured_max_parallel_read_steps(
    config: &sigil_kernel::TaskConfig,
) -> usize {
    config.max_parallel_read_steps.max(1)
}

pub(in crate::runner) fn configured_max_parallel_changeset_steps(
    config: &sigil_kernel::TaskConfig,
) -> usize {
    config.max_parallel_changeset_steps.max(1)
}

pub(in crate::runner) fn configured_provider_route_concurrency_limit(
    config: &sigil_kernel::TaskConfig,
) -> usize {
    configured_max_parallel_read_steps(config).max(configured_max_parallel_changeset_steps(config))
}

pub(in crate::runner) fn skill_child_agent_role(skill: &SkillDescriptor) -> AgentRole {
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

pub(in crate::runner) fn normalized_skill_agent_hint(agent: &str) -> String {
    agent
        .chars()
        .filter(|value| value.is_ascii_alphanumeric())
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

pub(in crate::runner) fn load_worker_skill(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    skill_id: &str,
    run_id: Option<u64>,
) -> std::result::Result<sigil_runtime::LoadedSkillContext, String> {
    let user_config_dir = default_user_config_dir().ok();
    let report = sigil_runtime::discover_skill_index_with_user_dir(
        &options.workspace_root,
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

pub(in crate::runner) fn skill_invocation_prompt(skill_id: &str, arguments: &str) -> String {
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

pub(in crate::runner) fn skill_child_session_objective(skill_id: &str, arguments: &str) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return format!("invoke agent {skill_id}");
    }
    format!("invoke agent {skill_id} with arguments: {trimmed}")
}

pub(in crate::runner) fn send_task_result(
    run_id: u64,
    session: Session,
    task_id: String,
    result: std::result::Result<TaskRunStatus, String>,
    task_result_tx: WorkerEventPayloadSender<RunTaskResult>,
) {
    let _ = task_result_tx.send(RunTaskResult {
        run_id,
        session,
        payload: RunTaskPayload::Task {
            task_id,
            queue_id: None,
            result,
        },
        post_run_maintenance: None,
    });
}

pub(in crate::runner) fn append_plan_draft(
    root_config: &RootConfig,
    workspace_root: &Path,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    final_text: &str,
    final_message_id: Option<String>,
    run_id: u64,
) -> std::result::Result<Option<PlanDraftCreatedEntry>, String> {
    let Some(session) = current_session.as_mut() else {
        return Err("session state is unavailable for plan artifact".to_owned());
    };
    let session_ref = session_ref_for_log_path(current_session_log_path)?
        .as_path()
        .display()
        .to_string();
    let source = PlanSourceRef {
        session_ref: Some(session_ref),
        run_id: Some(run_id.to_string()),
        final_message_id,
        ..PlanSourceRef::default()
    };
    let created_at_ms = current_unix_time_ms();
    let workspace_snapshot_id = match plan_handoff_workspace_snapshot_id(
        root_config,
        workspace_root,
    ) {
        Ok(workspace_snapshot_id) => workspace_snapshot_id,
        Err(error) => {
            tracing::warn!(%error, "plan workspace snapshot unavailable; continuing without it");
            None
        }
    };
    let structured_entry = plan_draft_created_entry(
        final_text,
        source.clone(),
        created_at_ms,
        workspace_snapshot_id.clone(),
    )
    .map_err(|error| format!("failed to build structured plan artifact: {error:#}"))?;
    let plain_entry = || {
        plain_text_plan_draft_entry(final_text, source, created_at_ms, workspace_snapshot_id)
            .map_err(|error| format!("failed to build plain-text plan artifact: {error:#}"))
    };
    let entry = match structured_entry {
        Some(entry) => Some(entry),
        None => plain_entry()?,
    };
    let Some(entry) = entry else {
        return Ok(None);
    };
    session
        .append_control(ControlEntry::PlanDraftCreated(entry.clone()))
        .map_err(|error| format!("failed to append plan artifact: {error:#}"))?;
    Ok(Some(entry))
}

pub(in crate::runner) type RejectPlanRequest = sigil_runtime::RejectPlanRequest;

/// RFC-0067 result of one typed Run command plus its first admission attempt.
pub(in crate::runner) struct AdoptedPlanRun {
    pub(in crate::runner) receipt: sigil_runtime::PlanApprovalReceiptV2,
    pub(in crate::runner) admission: sigil_kernel::TaskAdmissionOutcomeV1,
    pub(in crate::runner) entry: sigil_kernel::TaskCreatedFromPlanEntry,
    pub(in crate::runner) entries: Vec<sigil_kernel::SessionLogEntry>,
}

/// RFC-0067 single execution spine entry for every TUI Run surface (RFC-0067 6.5, 9).
///
/// Constructs the typed command and atomically creates the approved Task's first-class direct
/// execution admission. The runner starts immediately; no placeholder TaskPlan or model-authored
/// materialization is part of the Run action.
fn start_mode_str(mode: sigil_kernel::PlanTaskStartMode) -> &'static str {
    match mode {
        sigil_kernel::PlanTaskStartMode::CreatePaused => "paused",
        sigil_kernel::PlanTaskStartMode::CreateAndRun => "run",
    }
}

fn permission_choice_str(permission: Option<sigil_kernel::PlanApprovalPermission>) -> &'static str {
    match permission {
        Some(sigil_kernel::PlanApprovalPermission::WorkspaceEdits) => "scoped_edits",
        Some(sigil_kernel::PlanApprovalPermission::Ask) | None => "current_policy",
    }
}

pub(in crate::runner) fn adopt_plan_run(
    _root_config: &RootConfig,
    _workspace_root: &Path,
    session_log_path: &Path,
    session: &mut Session,
    plan_id: String,
    expected_plan_hash: String,
    start_mode: sigil_kernel::PlanTaskStartMode,
    permission_grant: Option<sigil_kernel::PlanApprovalPermission>,
    source: sigil_kernel::PlanRunCommandSource,
    _tool_contracts: Option<Vec<sigil_kernel::ToolRuntimeContract>>,
) -> std::result::Result<AdoptedPlanRun, String> {
    let parent_session_ref = session_ref_for_log_path(session_log_path)?;
    let plan_id = PlanId::new(plan_id).map_err(|error| format!("invalid plan id: {error}"))?;
    let command = sigil_kernel::PlanRunCommandV1 {
        command_id: sigil_kernel::stable_event_uuid(
            "sigil-plan-run-command-v1",
            &format!(
                "{}:{}:{}:{}:{}",
                session.session_scope_id(),
                plan_id.as_str(),
                expected_plan_hash,
                start_mode_str(start_mode),
                permission_choice_str(permission_grant),
            ),
        ),
        session_id: session.session_scope_id().to_owned(),
        plan_id: plan_id.clone(),
        expected_plan_hash,
        // RFC-0069 approval binds the reviewable Plan, not an advisory precompile cache.
        expected_candidate_hash: String::new(),
        expected_durable_frontier: session.durable_frontier_sequence(),
        start_mode,
        permission: match permission_grant {
            Some(sigil_kernel::PlanApprovalPermission::WorkspaceEdits) => {
                sigil_kernel::PlanRunPermissionChoiceV1::GrantScopedEditsOnce
            }
            Some(sigil_kernel::PlanApprovalPermission::Ask) | None => {
                sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy
            }
        },
        source,
    };
    let receipt = sigil_runtime::PlanExecutionService::approve(
        session,
        parent_session_ref,
        &command,
        current_unix_time_ms(),
    )
    .map_err(|rejection| sigil_runtime::plan_run_rejection_message(&rejection))?;
    let admission = sigil_runtime::PlanExecutionService::direct_execution_outcome(
        &receipt,
        current_unix_time_ms(),
    );
    let entry = session
        .plan_artifact_projection()
        .tasks_created
        .get(&plan_id)
        .and_then(|entries| entries.last())
        .cloned()
        .ok_or_else(|| "the adopted task link is unavailable".to_owned())?;
    let entries = session.entries().to_vec();
    Ok(AdoptedPlanRun {
        receipt,
        admission,
        entry,
        entries,
    })
}

pub(in crate::runner) fn plan_handoff_workspace_snapshot_id(
    root_config: &RootConfig,
    workspace_root: &Path,
) -> std::result::Result<Option<String>, String> {
    sigil_runtime::plan_handoff_workspace_snapshot_id(root_config, workspace_root)
        .map_err(|error| format!("{error:#}"))
}

pub(in crate::runner) fn reject_plan(
    root_config: &RootConfig,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    request: RejectPlanRequest,
) -> std::result::Result<(PlanDecisionRecordedEntry, Vec<SessionLogEntry>), String> {
    let mut session = load_session_with_runtime_attachments(
        &root_config.agent.runtime_provider,
        &root_config.agent.model,
        current_session_log_path,
        current_session.as_ref(),
    )
    .map_err(|error| format!("failed to load session before rejecting plan: {error:#}"))?;
    let rejected = match sigil_runtime::PlanReviewCoordinator::reject_plan(&mut session, &request) {
        Ok(rejected) => rejected,
        Err(error) => {
            *current_session = Some(session);
            return Err(format!("{error:#}"));
        }
    };
    *current_session = Some(session);
    Ok((rejected.entry, rejected.entries))
}

pub(in crate::runner) struct RequestedPlanRevisionGuidance {
    pub(in crate::runner) request: sigil_kernel::PublicUserInputRequestV1,
    pub(in crate::runner) entries: Vec<SessionLogEntry>,
}

pub(in crate::runner) fn revise_plan(
    root_config: &RootConfig,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    request: RejectPlanRequest,
) -> std::result::Result<RequestedPlanRevisionGuidance, String> {
    let plan_id = PlanId::new(request.plan_id.clone())
        .map_err(|error| format!("invalid plan id for revision: {error}"))?;
    let mut session = load_session_with_runtime_attachments(
        &root_config.agent.runtime_provider,
        &root_config.agent.model,
        current_session_log_path,
        current_session.as_ref(),
    )
    .map_err(|error| format!("failed to load session before revising plan: {error:#}"))?;
    let revision_request =
        match sigil_runtime::PlanReviewCoordinator::request_plan_revision_guidance(
            &mut session,
            &plan_id,
            &request.expected_plan_hash,
            current_unix_time_ms(),
        ) {
            Ok(request) => request,
            Err(error) => {
                *current_session = Some(session);
                return Err(format!("{error:#}"));
            }
        };
    let public = session
        .user_input_projection()
        .map_err(|error| format!("failed to project revision guidance: {error:#}"))?
        .request(&revision_request.request.identity)
        .cloned()
        .ok_or_else(|| "durable revision guidance request is unavailable".to_owned())?
        .public_view();
    let entries = session.entries().to_vec();
    *current_session = Some(session);
    Ok(RequestedPlanRevisionGuidance {
        request: public,
        entries,
    })
}

pub(in crate::runner) fn append_paused_task_state(
    session: &mut Session,
    task_id: &str,
) -> std::result::Result<(), String> {
    let task_id = TaskId::new(task_id.to_owned())
        .map_err(|error| format!("invalid active task id: {error}"))?;
    sigil_runtime::agent_supervisor::task_execution::append_task_stop_state(
        session,
        Some(&task_id),
        sigil_runtime::agent_supervisor::task_execution::TaskStopDisposition::Paused,
        "task paused from TUI",
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

pub(in crate::runner) fn append_interrupted_task_state(
    session: &mut Session,
    task_id: Option<&str>,
    reason: &str,
) -> std::result::Result<(), String> {
    let task_id = task_id
        .map(|value| {
            TaskId::new(value.to_owned())
                .map_err(|error| format!("invalid active task id: {error}"))
        })
        .transpose()?;
    sigil_runtime::agent_supervisor::task_execution::append_task_stop_state(
        session,
        task_id.as_ref(),
        sigil_runtime::agent_supervisor::task_execution::TaskStopDisposition::Interrupted,
        reason,
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

pub(in crate::runner) fn session_ref_for_log_path(
    path: &Path,
) -> std::result::Result<SessionRef, String> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("session.jsonl");
    SessionRef::new_relative(file_name)
        .map_err(|error| format!("failed to build parent session ref: {error:#}"))
}

pub(in crate::runner) fn plan_mode_transient_context(prompt: String) -> Vec<ModelMessage> {
    vec![
        ModelMessage::system(
            "Plan mode is active for this turn. Research, inspect, and propose a concrete execution plan, but do not modify files, run write-capable tools, or execute the plan. Use read-only tools and read-only agent delegation when helpful. If and only if you have a concrete executable plan, end with a fenced ```sigil-plan-v2 JSON block containing summary, steps, target_paths, suggested_checks, risk, and notes. Each step must include id, title, role, depends_on, mode, isolation, target_paths, suggested_checks, notes, and acceptance; detail, display_name, risk, and intent_aliases are optional. Use the same role/mode/isolation values as task_plan_update. Use [] for empty arrays. Dependencies must reference step ids in the same block. When the requested outcome contains multiple independently meaningful product or user outcomes that should remain separately reviewable or removable, use your semantic judgment to add a top-level intents array. Each intent must contain intent_alias, title, statement, acceptance_criteria, and depends_on_aliases; each criterion must contain criterion_alias, statement, and required. Bind affected steps with intent_aliases. Every write step in an intent-enabled plan must bind exactly one alias; read and review steps may bind zero or more. Do not create intents by mechanically copying every implementation step. Omit intents and intent_aliases when semantic decomposition would not help. Provider aliases are descriptive only: never emit runtime intent ids, stack versions, acceptance authority, or permission claims. If you are only summarizing, reviewing, or cannot produce executable steps, do not include a structured block.",
        ),
        ModelMessage::user(prompt),
    ]
}

pub(in crate::runner) fn next_task_id(session: &Session) -> std::result::Result<TaskId, String> {
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

pub(in crate::runner) fn resolve_continue_task(
    session: &Session,
    requested_task_id: Option<String>,
) -> std::result::Result<(TaskId, String, String, bool), String> {
    let task = sigil_runtime::agent_supervisor::task_execution::resolve_task_continuation(
        session,
        requested_task_id.as_deref(),
    )
    .map_err(|error| format!("{error:#}"))?;
    let needs_planning = task.needs_planning();
    Ok((
        task.task_id.clone(),
        task.task_id.as_str().to_owned(),
        task.objective,
        needs_planning,
    ))
}
