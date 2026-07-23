use super::*;

pub(in crate::runner) trait TaskRoleProviderBuilder: Send + Sync {
    fn build(
        &self,
        root_config: &RootConfig,
        role: AgentRole,
    ) -> std::result::Result<Box<dyn sigil_kernel::Provider>, String>;
}

/// Default role-provider builder used by product runtime paths.
///
/// The trait seam exists so runner tests can exercise task orchestration with deterministic
/// providers without registering a fake provider in `sigil-runtime`.
pub(in crate::runner) struct RuntimeTaskRoleProviderBuilder;

impl TaskRoleProviderBuilder for RuntimeTaskRoleProviderBuilder {
    fn build(
        &self,
        root_config: &RootConfig,
        role: AgentRole,
    ) -> std::result::Result<Box<dyn sigil_kernel::Provider>, String> {
        sigil_runtime::build_role_provider(root_config, role).map_err(|error| format!("{error:#}"))
    }
}

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
    pub(in crate::runner) task_result_tx: mpsc::Sender<RunTaskResult>,
    pub(in crate::runner) approval_rx: mpsc::Receiver<ApprovalSignal>,
    pub(in crate::runner) handler: ChannelEventHandler,
    pub(in crate::runner) elicitation_audit_buffer: McpElicitationAuditBuffer,
    pub(in crate::runner) cancellation_handle: RunCancellationHandle,
    pub(in crate::runner) cancellation_task_guard: RunTaskGuard,
}

pub(in crate::runner) struct TaskContinueSpawn {
    pub(in crate::runner) run_id: u64,
    pub(in crate::runner) session: Session,
    pub(in crate::runner) task_id: TaskId,
    pub(in crate::runner) task_id_value: String,
    pub(in crate::runner) parent_session_ref: SessionRef,
    pub(in crate::runner) objective: String,
    pub(in crate::runner) guidance: Option<String>,
    pub(in crate::runner) root_config: RootConfig,
    pub(in crate::runner) options: AgentRunOptions,
    pub(in crate::runner) base_registry: ToolRegistry,
    pub(in crate::runner) agent_supervisor: sigil_runtime::AgentSupervisor,
    pub(in crate::runner) role_provider_builder: Arc<dyn TaskRoleProviderBuilder>,
    pub(in crate::runner) task_result_tx: mpsc::Sender<RunTaskResult>,
    pub(in crate::runner) approval_rx: mpsc::Receiver<ApprovalSignal>,
    pub(in crate::runner) handler: ChannelEventHandler,
    pub(in crate::runner) elicitation_audit_buffer: McpElicitationAuditBuffer,
    pub(in crate::runner) cancellation_handle: RunCancellationHandle,
    pub(in crate::runner) cancellation_task_guard: RunTaskGuard,
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
    pub(in crate::runner) task_result_tx: mpsc::Sender<RunTaskResult>,
    pub(in crate::runner) approval_rx: mpsc::Receiver<ApprovalSignal>,
    pub(in crate::runner) handler: ChannelEventHandler,
    pub(in crate::runner) elicitation_audit_buffer: McpElicitationAuditBuffer,
    pub(in crate::runner) cancellation_handle: RunCancellationHandle,
    pub(in crate::runner) cancellation_task_guard: RunTaskGuard,
}

pub(in crate::runner) struct TaskRoleRuntime {
    pub(in crate::runner) orchestrator:
        SequentialTaskOrchestrator<sigil_runtime::AgentSupervisorTaskChildRunner>,
    pub(in crate::runner) planner_options: AgentRunOptions,
    pub(in crate::runner) executor_options: AgentRunOptions,
    pub(in crate::runner) subagent_read_options: AgentRunOptions,
    pub(in crate::runner) subagent_write_options: AgentRunOptions,
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
        } = spawn;
        let _cancellation_task_guard = cancellation_task_guard;
        let terminal_cancellation = cancellation_handle.clone();
        let terminal_task_id = task_id.clone();
        let terminal_parent_session_ref = parent_session_ref.clone();
        let terminal_objective = objective.clone();
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
                role_provider_builder: role_provider_builder.as_ref(),
                approval_rx,
                handler: &mut handler,
                cancellation_handle,
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
}

pub(in crate::runner) struct TaskContinueOrchestration<'a> {
    task_id: TaskId,
    parent_session_ref: SessionRef,
    objective: String,
    guidance: Option<String>,
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    role_provider_builder: &'a dyn TaskRoleProviderBuilder,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: &'a mut ChannelEventHandler,
    cancellation_handle: RunCancellationHandle,
}

pub(in crate::runner) async fn run_task_orchestration(
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
        role_provider_builder,
        approval_rx,
        handler,
        cancellation_handle,
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
        },
        &mut approval_handler,
    )
    .await
}

pub(in crate::runner) async fn run_admitted_task_orchestration<A>(
    session: &mut Session,
    request: AdmittedTaskRunOrchestration<'_>,
    approval_handler: &mut A,
) -> std::result::Result<TaskRunStatus, String>
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
    } = build_task_role_runtime(
        &root_config,
        &options,
        &base_registry,
        agent_supervisor,
        role_provider_builder,
    )?;
    let orchestrator = orchestrator.with_cancellation(cancellation_handle);
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
            approval_handler,
        )
        .await
        .map(|output| output.status)
        .map_err(|error| format!("{error:#}"))
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

fn finalize_task_root(
    session: &mut Session,
    task_id: &TaskId,
    parent_session_ref: &SessionRef,
    objective: &str,
    terminal_cancellation: &RunCancellationHandle,
    result: std::result::Result<TaskRunStatus, String>,
) -> std::result::Result<TaskRunStatus, String> {
    if !terminal_cancellation.is_naturally_finalized()
        && !terminal_cancellation.try_finalize_naturally()
    {
        return Err("run cancellation won the task terminal-state race".to_owned());
    }
    let Err(error) = &result else {
        return result;
    };
    let status = session
        .task_state_projection()
        .tasks
        .get(task_id)
        .map(|task| task.status);
    if matches!(
        status,
        Some(TaskRunStatus::Started | TaskRunStatus::Running)
    ) {
        session
            .append_control(ControlEntry::TaskRun(TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: parent_session_ref.clone(),
                objective: sigil_kernel::safe_persistence_text(objective),
                status: TaskRunStatus::Failed,
                reason: Some(sigil_kernel::safe_persistence_text(&format!(
                    "task orchestration failed before a terminal state: {error}"
                ))),
            }))
            .map_err(|append_error| {
                format!("failed to persist task orchestration failure: {append_error:#}")
            })?;
    }
    result
}

pub(in crate::runner) async fn continue_task_orchestration(
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
        role_provider_builder,
        approval_rx,
        handler,
        cancellation_handle,
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
    } = build_task_role_runtime(
        &root_config,
        &options,
        &base_registry,
        agent_supervisor,
        role_provider_builder,
    )?;
    let orchestrator = orchestrator.with_cancellation(cancellation_handle);
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
    )?;
    let orchestrator = orchestrator.with_cancellation(cancellation_handle);
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

pub(in crate::runner) fn materialize_task_verification_config(
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

pub(in crate::runner) fn workspace_promoted_check_for_candidate(
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

pub(in crate::runner) fn check_spec_entries_workspace_trust_requirement(
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

pub(in crate::runner) fn build_task_role_runtime(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    base_registry: &ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    role_provider_builder: &dyn TaskRoleProviderBuilder,
) -> std::result::Result<TaskRoleRuntime, String> {
    let planner_provider = role_provider_builder.build(root_config, AgentRole::Planner)?;
    let executor_provider = role_provider_builder.build(root_config, AgentRole::Executor)?;
    let synthesis_provider = role_provider_builder.build(root_config, AgentRole::Planner)?;
    let subagent_read_provider =
        role_provider_builder.build(root_config, AgentRole::SubagentRead)?;
    let subagent_write_provider =
        role_provider_builder.build(root_config, AgentRole::SubagentWrite)?;
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
    );
    let execution_backend = sigil_runtime::build_configured_execution_backend(root_config)
        .map_err(|error| format!("failed to build verification execution backend: {error:#}"))?;
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

pub(in crate::runner) fn build_skill_child_role_runtime(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    base_registry: &ToolRegistry,
    skill: &SkillDescriptor,
    child_role: AgentRole,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    role_provider_builder: &dyn TaskRoleProviderBuilder,
) -> std::result::Result<TaskRoleRuntime, String> {
    let planner_provider = role_provider_builder.build(root_config, AgentRole::Planner)?;
    let executor_provider = role_provider_builder.build(root_config, AgentRole::Executor)?;
    let synthesis_provider = role_provider_builder.build(root_config, AgentRole::Planner)?;
    let subagent_read_provider =
        role_provider_builder.build(root_config, AgentRole::SubagentRead)?;
    let subagent_write_provider =
        role_provider_builder.build(root_config, AgentRole::SubagentWrite)?;
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
    );
    let execution_backend = sigil_runtime::build_configured_execution_backend(root_config)
        .map_err(|error| format!("failed to build verification execution backend: {error:#}"))?;
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
    task_result_tx: mpsc::Sender<RunTaskResult>,
) {
    let _ = task_result_tx.send(RunTaskResult {
        run_id,
        session,
        payload: RunTaskPayload::Task {
            task_id,
            queue_id: None,
            result,
        },
    });
}

pub(in crate::runner) struct PlanApprovalRequest {
    pub(in crate::runner) plan_text: String,
    pub(in crate::runner) permission: PlanApprovalPermission,
    pub(in crate::runner) scope_summary: String,
    pub(in crate::runner) clear_planning_context: bool,
}

pub(in crate::runner) fn approve_plan(
    root_config: &RootConfig,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    request: PlanApprovalRequest,
) -> std::result::Result<(PlanApprovedEntry, Vec<SessionLogEntry>), String> {
    let safe_plan_text = sigil_kernel::safe_persistence_text(&request.plan_text);
    let plan_text = safe_plan_text.trim();
    if plan_text.is_empty() {
        return Err("plan approval failed: plan text is empty".to_owned());
    }
    let mut session = load_session_with_runtime_attachments(
        &root_config.agent.provider,
        &root_config.agent.model,
        current_session_log_path,
        current_session.as_ref(),
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
        sigil_kernel::safe_persistence_text(request.scope_summary.trim())
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
    };
    let workspace_snapshot_id = plan_handoff_workspace_snapshot_id(root_config, workspace_root)
        .map_err(|error| format!("failed to build plan workspace snapshot: {error}"))?;
    let Some(entry) = plan_draft_created_entry(
        final_text,
        source,
        current_unix_time_ms(),
        workspace_snapshot_id,
    )
    .map_err(|error| format!("failed to build plan artifact: {error:#}"))?
    else {
        return Ok(None);
    };
    session
        .append_control(ControlEntry::PlanDraftCreated(entry.clone()))
        .map_err(|error| format!("failed to append plan artifact: {error:#}"))?;
    Ok(Some(entry))
}

pub(in crate::runner) struct CreateTaskFromPlanRequest {
    pub(in crate::runner) plan_id: String,
    pub(in crate::runner) expected_plan_hash: String,
    pub(in crate::runner) start_mode: PlanTaskStartMode,
    pub(in crate::runner) permission_grant: Option<PlanApprovalPermission>,
}

pub(in crate::runner) struct RejectPlanRequest {
    pub(in crate::runner) plan_id: String,
    pub(in crate::runner) expected_plan_hash: String,
}

pub(in crate::runner) struct CreatedTaskFromPlan {
    pub(in crate::runner) task_id: TaskId,
    pub(in crate::runner) task_id_value: String,
    pub(in crate::runner) objective: String,
    pub(in crate::runner) entry: TaskCreatedFromPlanEntry,
    pub(in crate::runner) start_mode: PlanTaskStartMode,
    pub(in crate::runner) entries: Vec<SessionLogEntry>,
}

pub(in crate::runner) fn plan_handoff_workspace_snapshot_id(
    root_config: &RootConfig,
    workspace_root: &Path,
) -> std::result::Result<Option<String>, String> {
    let workspace_id = stable_workspace_id(workspace_root).map_err(|error| format!("{error:#}"))?;
    let scope = root_config
        .verification
        .scope_for_hash(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    let snapshot = build_workspace_snapshot(workspace_root, workspace_id, &scope, 0)
        .map_err(|error| format!("{error:#}"))?;
    Ok(snapshot.workspace_snapshot_id)
}

fn plan_handoff_stale_reason(
    base_workspace_snapshot_id: Option<&str>,
    current_workspace_snapshot_id: Option<&str>,
) -> Option<String> {
    match (base_workspace_snapshot_id, current_workspace_snapshot_id) {
        (Some(base), Some(current)) => (base != current).then(|| {
            format!(
                "plan may be stale: workspace changed since plan was created (base={}, current={})",
                truncate_plan_snapshot_id(base),
                truncate_plan_snapshot_id(current)
            )
        }),
        (Some(base), None) => Some(format!(
            "plan may be stale: current workspace snapshot is unavailable (base={})",
            truncate_plan_snapshot_id(base)
        )),
        (None, _) => Some(
            "plan cannot be direct-promoted: its base workspace snapshot is unavailable".to_owned(),
        ),
    }
}

fn truncate_plan_snapshot_id(snapshot_id: &str) -> String {
    snapshot_id.chars().take(24).collect()
}

pub(in crate::runner) fn create_task_from_plan(
    root_config: &RootConfig,
    workspace_root: &Path,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    request: CreateTaskFromPlanRequest,
) -> std::result::Result<CreatedTaskFromPlan, String> {
    let plan_id = PlanId::new(request.plan_id.clone())
        .map_err(|error| format!("invalid plan id for task creation: {error:#}"))?;
    let mut session = load_session_with_runtime_attachments(
        &root_config.agent.provider,
        &root_config.agent.model,
        current_session_log_path,
        current_session.as_ref(),
    )
    .map_err(|error| format!("failed to load session before creating task from plan: {error:#}"))?;
    let projection = session.plan_artifact_projection();
    let draft = projection
        .plans
        .get(&plan_id)
        .cloned()
        .ok_or_else(|| format!("plan {} is not present in this session", plan_id.as_str()))?;
    if draft.plan_hash != request.expected_plan_hash {
        return Err(format!(
            "plan {} is stale: expected {}, current {}",
            plan_id.as_str(),
            request.expected_plan_hash,
            draft.plan_hash
        ));
    }
    if projection.plan_is_rejected(&plan_id) {
        return Err(format!("plan {} was rejected", plan_id.as_str()));
    }
    let current_workspace_snapshot_id =
        plan_handoff_workspace_snapshot_id(root_config, workspace_root)
            .map_err(|error| format!("failed to build current workspace snapshot: {error}"))?;
    let stale_reason = plan_handoff_stale_reason(
        draft.workspace_snapshot_id.as_deref(),
        current_workspace_snapshot_id.as_deref(),
    );
    let task_id = task_id_from_plan_draft(&draft)
        .map_err(|error| format!("failed to derive stable task id from plan: {error:#}"))?;
    let task_id_value = task_id.as_str().to_owned();
    let parent_session_ref = session_ref_for_log_path(current_session_log_path)?;
    let objective = plan_task_input_from_draft(&draft);
    let decision = PlanDecisionRecordedEntry {
        plan_id: plan_id.clone(),
        plan_hash: draft.plan_hash.clone(),
        decision: PlanDecision::Accepted,
        decided_by: PlanDecisionActor::User,
        decided_at_ms: current_unix_time_ms(),
        reason: Some("created task from plan".to_owned()),
    };
    if draft.steps.len() > root_config.task.max_plan_steps {
        return Err(format!(
            "plan {} has {} steps, exceeding task.max_plan_steps={}",
            plan_id.as_str(),
            draft.steps.len(),
            root_config.task.max_plan_steps
        ));
    }
    let promoted = if stale_reason.is_none() {
        task_plan_from_plan_draft(&draft, task_id.clone(), 1)
            .map_err(|error| format!("approved plan cannot be promoted safely: {error:#}"))?
    } else {
        None
    };
    let (task_plan, step_mapping) = match promoted {
        Some((plan, mapping)) => (Some(plan), mapping),
        None => (None, Vec::new()),
    };
    let existing_accepted_plan = session
        .task_state_projection()
        .tasks
        .get(&task_id)
        .and_then(|task| {
            task.plans
                .values()
                .find(|plan| plan.status == TaskPlanStatus::Accepted)
        })
        .cloned();
    if task_plan.is_none()
        && let Some(existing_plan) = existing_accepted_plan
    {
        session
            .append_control(ControlEntry::TaskPlan(TaskPlanEntry {
                task_id: task_id.clone(),
                plan_version: existing_plan.plan_version,
                status: TaskPlanStatus::Superseded,
                steps: existing_plan.steps,
                reason: Some(
                    "workspace drift invalidated a crash-interrupted plan promotion".to_owned(),
                ),
            }))
            .map_err(|error| format!("failed to supersede stale promoted task plan: {error:#}"))?;
        let existing_task = session
            .task_state_projection()
            .tasks
            .get(&task_id)
            .cloned()
            .ok_or_else(|| "stale promoted task prefix is missing its task run".to_owned())?;
        session
            .append_control(ControlEntry::TaskRun(TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: existing_task.parent_session_ref,
                objective: existing_task.objective,
                status: TaskRunStatus::Cancelled,
                reason: Some(
                    "plan creation cancelled because the workspace changed before commit"
                        .to_owned(),
                ),
            }))
            .map_err(|error| format!("failed to cancel stale promoted task prefix: {error:#}"))?;
        *current_session = Some(session);
        return Err(format!(
            "plan {} creation prefix conflicts with current workspace drift; refusing to execute an earlier promoted task plan",
            plan_id.as_str()
        ));
    }
    let task_created = TaskCreatedFromPlanEntry {
        plan_id: plan_id.clone(),
        plan_hash: draft.plan_hash.clone(),
        task_id: task_id.clone(),
        task_plan_version: task_plan.as_ref().map_or(0, |plan| plan.plan_version),
        step_mapping,
        stale_reason,
        created_at_ms: current_unix_time_ms(),
    };
    let permission_grant = match request.permission_grant {
        Some(permission) => {
            if draft.target_paths.is_empty() {
                return Err(format!(
                    "plan {} has no concrete target paths for scoped edits",
                    plan_id.as_str()
                ));
            }
            Some(PlanPermissionGrantedEntry {
                plan_id: plan_id.clone(),
                plan_hash: draft.plan_hash.clone(),
                task_id: task_id.clone(),
                workspace_snapshot_id: current_workspace_snapshot_id,
                permission,
                scope: PlanApprovalScope {
                    summary: format!("scoped edits for task {}", task_id.as_str()),
                    workspace_paths: draft.target_paths.clone(),
                },
                expires: PlanApprovalExpiry::Session,
                granted_at_ms: current_unix_time_ms(),
            })
        }
        None => None,
    };

    let desired_task_status = if request.start_mode == PlanTaskStartMode::CreatePaused {
        TaskRunStatus::Paused
    } else {
        TaskRunStatus::Started
    };
    let safe_objective = sigil_kernel::safe_persistence_text(&objective);
    let existing_task = session.task_state_projection().tasks.get(&task_id).cloned();
    match existing_task {
        Some(existing)
            if existing.parent_session_ref == parent_session_ref
                && existing.objective == safe_objective
                && existing.status == desired_task_status => {}
        Some(existing)
            if existing.parent_session_ref == parent_session_ref
                && existing.objective == safe_objective
                && existing.status == TaskRunStatus::Paused
                && desired_task_status == TaskRunStatus::Started
                && existing.participant_attempts.is_empty()
                && existing.steps.is_empty() =>
        {
            session
                .append_control(ControlEntry::TaskRun(TaskRunEntry {
                    task_id: task_id.clone(),
                    parent_session_ref: parent_session_ref.clone(),
                    objective: safe_objective.clone(),
                    status: TaskRunStatus::Started,
                    reason: Some(format!(
                        "resumed crash-interrupted creation from plan {}",
                        plan_id.as_str()
                    )),
                }))
                .map_err(|error| format!("failed to resume task-from-plan prefix: {error:#}"))?;
        }
        Some(_) => {
            return Err(format!(
                "plan {} task prefix conflicts with the requested task facts",
                plan_id.as_str()
            ));
        }
        None => session
            .append_control(ControlEntry::TaskRun(TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: parent_session_ref.clone(),
                objective: safe_objective,
                status: desired_task_status,
                reason: Some(format!("created from plan {}", plan_id.as_str())),
            }))
            .map_err(|error| format!("failed to append task-from-plan run: {error:#}"))?,
    }

    if let Some(task_plan) = task_plan {
        let existing_plan = session
            .task_state_projection()
            .tasks
            .get(&task_id)
            .and_then(|task| task.plans.get(&task_plan.plan_version))
            .cloned();
        match existing_plan {
            Some(existing)
                if existing.plan_version == task_plan.plan_version
                    && existing.status == task_plan.status
                    && existing.steps == task_plan.steps
                    && existing.reason == task_plan.reason => {}
            Some(_) => {
                return Err(format!(
                    "plan {} task-plan prefix conflicts with direct promotion",
                    plan_id.as_str()
                ));
            }
            None => session
                .append_control(ControlEntry::TaskPlan(task_plan))
                .map_err(|error| format!("failed to append promoted task plan: {error:#}"))?,
        }
    }

    if let Some(grant) = permission_grant {
        let existing_grants = session
            .plan_artifact_projection()
            .permission_grants
            .get(&plan_id)
            .cloned()
            .unwrap_or_default();
        if existing_grants.iter().any(|existing| {
            existing.plan_hash == grant.plan_hash
                && existing.task_id == grant.task_id
                && existing.workspace_snapshot_id == grant.workspace_snapshot_id
                && existing.permission == grant.permission
                && existing.scope == grant.scope
                && existing.expires == grant.expires
        }) {
            // The crash-prefix retry already persisted this exact grant.
        } else if existing_grants
            .iter()
            .any(|existing| existing.task_id == task_id)
        {
            return Err(format!(
                "plan {} already has a conflicting permission grant for this task",
                plan_id.as_str()
            ));
        } else {
            session
                .append_control(ControlEntry::PlanPermissionGranted(grant))
                .map_err(|error| format!("failed to append task permission grant: {error:#}"))?;
        }
    }

    let existing_created = session
        .plan_artifact_projection()
        .tasks_created
        .get(&plan_id)
        .and_then(|entries| entries.last())
        .cloned();
    match existing_created {
        Some(existing)
            if existing.plan_id == task_created.plan_id
                && existing.plan_hash == task_created.plan_hash
                && existing.task_id == task_created.task_id
                && existing.task_plan_version == task_created.task_plan_version
                && existing.step_mapping == task_created.step_mapping
                && existing.stale_reason == task_created.stale_reason => {}
        Some(_) => {
            return Err(format!(
                "plan {} already has a conflicting task-created anchor",
                plan_id.as_str()
            ));
        }
        None => session
            .append_control(ControlEntry::TaskCreatedFromPlan(task_created.clone()))
            .map_err(|error| format!("failed to append task-from-plan anchor: {error:#}"))?,
    }

    // Acceptance is the final commit marker. Until it is durable, the plan remains visible as a
    // pending handoff and another user action can reconcile the deterministic prefix above.
    let existing_decision = session
        .plan_artifact_projection()
        .latest_decision(&plan_id)
        .cloned();
    match existing_decision {
        Some(existing)
            if existing.decision == PlanDecision::Accepted
                && existing.plan_hash == draft.plan_hash => {}
        Some(existing) if existing.decision == PlanDecision::Accepted => {
            return Err(format!(
                "plan {} already has an accepted decision for another hash",
                plan_id.as_str()
            ));
        }
        _ => session
            .append_control(ControlEntry::PlanDecisionRecorded(decision))
            .map_err(|error| format!("failed to append plan acceptance: {error:#}"))?,
    }

    let entries = session.entries().to_vec();
    *current_session = Some(session);
    Ok(CreatedTaskFromPlan {
        task_id,
        task_id_value,
        objective,
        entry: task_created,
        start_mode: request.start_mode,
        entries,
    })
}

pub(in crate::runner) fn reject_plan(
    root_config: &RootConfig,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    request: RejectPlanRequest,
) -> std::result::Result<(PlanDecisionRecordedEntry, Vec<SessionLogEntry>), String> {
    let plan_id = PlanId::new(request.plan_id.clone())
        .map_err(|error| format!("invalid plan id for rejection: {error:#}"))?;
    let mut session = load_session_with_runtime_attachments(
        &root_config.agent.provider,
        &root_config.agent.model,
        current_session_log_path,
        current_session.as_ref(),
    )
    .map_err(|error| format!("failed to load session before rejecting plan: {error:#}"))?;
    let projection = session.plan_artifact_projection();
    let draft = projection
        .plans
        .get(&plan_id)
        .ok_or_else(|| format!("plan {} is not present in this session", plan_id.as_str()))?;
    if draft.plan_hash != request.expected_plan_hash {
        return Err(format!(
            "plan {} is stale: expected {}, current {}",
            plan_id.as_str(),
            request.expected_plan_hash,
            draft.plan_hash
        ));
    }
    if projection.task_created_for_plan(&plan_id) {
        return Err(format!("plan {} already created a task", plan_id.as_str()));
    }
    if let Some(decision) = projection.latest_decision(&plan_id) {
        return Err(format!(
            "plan {} already has decision {}",
            plan_id.as_str(),
            decision.decision.as_str()
        ));
    }

    let entry = PlanDecisionRecordedEntry {
        plan_id,
        plan_hash: draft.plan_hash.clone(),
        decision: PlanDecision::Rejected,
        decided_by: PlanDecisionActor::User,
        decided_at_ms: current_unix_time_ms(),
        reason: Some("discarded plan".to_owned()),
    };
    session
        .append_control(ControlEntry::PlanDecisionRecorded(entry.clone()))
        .map_err(|error| format!("failed to append plan rejection state: {error:#}"))?;
    let entries = session.entries().to_vec();
    *current_session = Some(session);
    Ok((entry, entries))
}

pub(in crate::runner) fn append_cancelled_task_state(
    session: &mut Session,
) -> std::result::Result<(), String> {
    append_terminated_task_state(
        session,
        TaskRunStatus::Cancelled,
        TaskStepStatus::Cancelled,
        TaskChildSessionStatus::Cancelled,
        "run cancelled from TUI",
    )
}

pub(in crate::runner) fn append_interrupted_task_state(
    session: &mut Session,
    reason: &str,
) -> std::result::Result<(), String> {
    append_terminated_task_state(
        session,
        TaskRunStatus::Interrupted,
        TaskStepStatus::Interrupted,
        TaskChildSessionStatus::Interrupted,
        reason,
    )
}

fn append_terminated_task_state(
    session: &mut Session,
    task_status: TaskRunStatus,
    step_status: TaskStepStatus,
    child_status: TaskChildSessionStatus,
    reason: &str,
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
    let active_steps = task
        .active_steps
        .iter()
        .filter_map(|key| task.steps.get(key))
        .filter(|step| !step.status.is_terminal())
        .cloned()
        .collect::<Vec<_>>();
    let child_cancellations = task
        .child_sessions
        .values()
        .filter(|child| child.status == TaskChildSessionStatus::Started)
        .cloned()
        .collect::<Vec<_>>();
    let _ = task;

    for step in active_steps {
        session
            .append_control(ControlEntry::TaskStep(TaskStepEntry {
                task_id: task_id.clone(),
                plan_version: step.plan_version,
                step_id: step.step_id,
                role: step.role,
                status: step_status,
                title: step
                    .title
                    .as_deref()
                    .map(sigil_kernel::safe_persistence_text),
                summary: None,
                reason: Some(sigil_kernel::safe_persistence_text(reason)),
            }))
            .map_err(|error| format!("failed to append cancelled task step: {error:#}"))?;
    }
    for mut child in child_cancellations {
        child.status = child_status;
        session
            .append_control(ControlEntry::TaskChildSession(child))
            .map_err(|error| format!("failed to append cancelled child session: {error:#}"))?;
    }
    session
        .append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id,
            parent_session_ref,
            objective: sigil_kernel::safe_persistence_text(&objective),
            status: task_status,
            reason: Some(sigil_kernel::safe_persistence_text(reason)),
        }))
        .map_err(|error| format!("failed to append cancelled task run: {error:#}"))?;
    Ok(())
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
            "Plan mode is active for this turn. Research, inspect, and propose a concrete execution plan, but do not modify files, run write-capable tools, or execute the plan. Use read-only tools and read-only agent delegation when helpful. If and only if you have a concrete executable plan, end with a fenced ```sigil-plan-v2 JSON block containing summary, steps, target_paths, suggested_checks, risk, and notes. Each step must include id, title, role, depends_on, mode, isolation, target_paths, suggested_checks, notes, and acceptance; detail, display_name, and risk are optional. Use the same role/mode/isolation values as task_plan_update. Use [] for empty arrays. Dependencies must reference step ids in the same block. If you are only summarizing, reviewing, or cannot produce executable steps, do not include a structured block.",
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
    let needs_planning = task.latest_plan_version.is_none();
    Ok((
        task.task_id.clone(),
        task.task_id.as_str().to_owned(),
        task.objective.clone(),
        needs_planning,
    ))
}
