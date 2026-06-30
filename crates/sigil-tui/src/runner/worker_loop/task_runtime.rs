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
    pub(in crate::runner) task_result_tx: mpsc::Sender<RunTaskResult>,
    pub(in crate::runner) approval_rx: mpsc::Receiver<ApprovalSignal>,
    pub(in crate::runner) handler: ChannelEventHandler,
    pub(in crate::runner) elicitation_audit_buffer: McpElicitationAuditBuffer,
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
    pub(in crate::runner) task_result_tx: mpsc::Sender<RunTaskResult>,
    pub(in crate::runner) approval_rx: mpsc::Receiver<ApprovalSignal>,
    pub(in crate::runner) handler: ChannelEventHandler,
    pub(in crate::runner) elicitation_audit_buffer: McpElicitationAuditBuffer,
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
    pub(in crate::runner) task_result_tx: mpsc::Sender<RunTaskResult>,
    pub(in crate::runner) approval_rx: mpsc::Receiver<ApprovalSignal>,
    pub(in crate::runner) handler: ChannelEventHandler,
    pub(in crate::runner) elicitation_audit_buffer: McpElicitationAuditBuffer,
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

pub(in crate::runner) struct TaskRunOrchestration<'a> {
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
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: &'a mut ChannelEventHandler,
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
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: &'a mut ChannelEventHandler,
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

pub(in crate::runner) fn build_skill_child_role_runtime(
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
        payload: RunTaskPayload::Task { task_id, result },
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

pub(in crate::runner) fn append_cancelled_task_state(
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
            "Plan mode is active for this turn. Research, inspect, and propose a concrete plan, but do not modify files, run write-capable tools, or execute the plan. Use read-only tools and read-only agent delegation when helpful. End with the plan and any open questions needed before implementation.",
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
