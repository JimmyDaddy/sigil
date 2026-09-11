//! Direct Task execution builds only its dispatched role and retains kernel completion blockers.
use super::*;
use crate::agent_supervisor::task_execution::{
    AdmittedTaskExecution, ContinuedTaskExecution, bind_task_run_cancellation_scope,
    continue_task_execution, finalize_task_root, run_admitted_task_to_root_terminal,
};

struct ExecutorOnlyBuilder {
    built: Arc<Mutex<Vec<AgentRole>>>,
    fail_executor: bool,
}

#[async_trait]
impl TaskRoleProviderBuilder for ExecutorOnlyBuilder {
    async fn build(&self, _config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        self.built.lock().expect("role capture").push(role);
        anyhow::ensure!(role == AgentRole::Executor, "unused role is unavailable");
        anyhow::ensure!(!self.fail_executor, "executor configuration is unavailable");
        Ok(Box::new(ApplicationTaskRoleProvider { role }))
    }
}

async fn run_direct_ablation_fixture(
    continued: bool,
    fail_executor: bool,
    active_participant: bool,
) -> Result<(Session, Vec<AgentRole>, TaskRunStatus)> {
    let root = tempfile::tempdir()?;
    let config_path = root.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let mut config = RootConfig::load(&config_path)?;
    config.task.enabled = true;
    config.memory = MemoryConfig::with_enabled(false);
    config.verification.checks.clear();
    // This untrusted candidate has never been promoted and cannot prevent provider dispatch.
    std::fs::write(
        root.path().join("package.json"),
        "{malformed-unused-package",
    )?;
    let mut session = Session::new("application-task-test", "application-task-model");
    let task_id = TaskId::new("direct-ablation")?;
    let parent = SessionRef::new_relative("session.jsonl")?;
    let objective = "Inspect the requested task";
    session.append_controls(vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: parent.clone(),
            objective: objective.to_owned(),
            title: None,
            status: if continued {
                TaskRunStatus::Paused
            } else {
                TaskRunStatus::Started
            },
            reason: None,
        }),
        ControlEntry::TaskDirectExecutionAdmittedV1(
            sigil_kernel::TaskDirectExecutionAdmittedV1::approved_plan(
                task_id.clone(),
                objective,
                sigil_kernel::PlanId::new("approved-direct-ablation")?,
                format!("sha256:{}", "a".repeat(64)),
                1,
            ),
        ),
    ])?;
    if active_participant {
        let attempt_id = sigil_kernel::task_participant_attempt_id(
            &task_id,
            sigil_kernel::TaskParticipantPurpose::Planner,
            None,
            None,
            1,
        )?;
        session.append_control(ControlEntry::TaskParticipantAttempt(
            sigil_kernel::TaskParticipantAttemptEntry {
                child_session_ref: sigil_kernel::task_participant_session_ref(
                    &task_id,
                    &attempt_id,
                )?,
                attempt_id,
                task_id: task_id.clone(),
                purpose: sigil_kernel::TaskParticipantPurpose::Planner,
                ordinal: 1,
                plan_version: None,
                step_id: None,
                role: AgentRole::Planner,
                status: sigil_kernel::TaskParticipantAttemptStatus::Started,
                reason: None,
            },
        ))?;
    }
    let registry = crate::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
        &config,
        root.path(),
        session.entries(),
    )?;
    let supervisor = crate::AgentSupervisor::new(
        registry,
        crate::AgentBudgetPolicy::from_root_config(&config),
        application_task_provider_capabilities(),
    );
    let options = crate::build_run_options(
        &config,
        root.path().to_path_buf(),
        InteractionMode::Headless,
        None,
    );
    let built = Arc::new(Mutex::new(Vec::new()));
    let builder = ExecutorOnlyBuilder {
        built: Arc::clone(&built),
        fail_executor,
    };
    let owner = RunCancellationOwner::new();
    let cancellation = owner.handle();
    bind_task_run_cancellation_scope(&mut session, &task_id, &cancellation)?;
    let status = if continued {
        let result = Box::pin(continue_task_execution(
            &mut session,
            ContinuedTaskExecution {
                requested_task_id: Some(task_id.clone()),
                guidance: None,
                explicit_guidance_run_id: None,
                guidance_promotion: None,
                continuation_guidance_receipt: None,
                root_config: config,
                options,
                base_registry: ToolRegistry::new(),
                agent_supervisor: supervisor,
                role_provider_builder: &builder,
                verification_execution_port: Arc::new(LocalExecutionBackend),
                handler: &mut NoopEventHandler,
                cancellation_handle: cancellation.clone(),
                tool_artifact_read_budget: None,
            },
            &mut AutoApproveHandler,
        ))
        .await;
        finalize_task_root(
            &mut session,
            &task_id,
            &parent,
            objective,
            &cancellation,
            result,
        )?
    } else {
        Box::pin(run_admitted_task_to_root_terminal(
            &mut session,
            AdmittedTaskExecution {
                task_id: task_id.clone(),
                parent_session_ref: parent,
                objective: objective.to_owned(),
                root_config: config,
                options,
                base_registry: ToolRegistry::new(),
                agent_supervisor: supervisor,
                role_provider_builder: &builder,
                verification_execution_port: Arc::new(LocalExecutionBackend),
                handler: &mut NoopEventHandler,
                cancellation_handle: cancellation,
                tool_artifact_read_budget: None,
            },
            &mut AutoApproveHandler,
        ))
        .await?
    };
    let built = built.lock().expect("role capture").clone();
    Ok((session, built, status))
}

#[tokio::test]
async fn direct_initial_and_continued_execution_ignore_unused_role_failures_and_untrusted_manifests()
-> Result<()> {
    for continued in [false, true] {
        let (session, built, status) = run_direct_ablation_fixture(continued, false, false).await?;
        assert_eq!(built, vec![AgentRole::Executor]);
        assert_eq!(status, TaskRunStatus::Completed);
        assert!(session.entries().iter().any(|entry| matches!(entry,
            SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAttemptV1(attempt))
                if attempt.status == sigil_kernel::TaskParticipantAttemptStatus::Completed
        )));
    }
    Ok(())
}

#[tokio::test]
async fn direct_executor_build_failure_stays_resumable_without_dispatch() -> Result<()> {
    for continued in [false, true] {
        let (session, built, status) = run_direct_ablation_fixture(continued, true, false).await?;
        assert_eq!(built, vec![AgentRole::Executor]);
        assert_eq!(status, TaskRunStatus::Paused);
        let projection = session.task_state_projection();
        let task = projection.latest_task().expect("durable Task");
        assert_eq!(
            task.reason.as_deref(),
            Some("task_role_runtime_preflight_blocked")
        );
        assert!(task.direct_execution_attempts.is_empty());
        assert!(task.participant_attempts.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn direct_final_answer_keeps_kernel_unfinished_participant_reason() -> Result<()> {
    let (session, built, status) = run_direct_ablation_fixture(false, false, true).await?;
    assert_eq!(built, vec![AgentRole::Executor]);
    assert_eq!(status, TaskRunStatus::Paused);
    let projection = session.task_state_projection();
    let task = projection.latest_task().expect("durable Task");
    assert!(
        task.reason
            .as_deref()
            .is_some_and(|reason| reason.contains("unfinished Task dependencies"))
    );
    assert!(
        task.direct_execution_attempts
            .values()
            .all(|attempt| attempt.status != sigil_kernel::TaskParticipantAttemptStatus::Completed)
    );
    Ok(())
}

struct InitialGuidanceProviderBuilder(Arc<Mutex<Vec<CompletionRequest>>>);
struct InitialGuidanceProvider {
    role: AgentRole,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl TaskRoleProviderBuilder for InitialGuidanceProviderBuilder {
    async fn build(&self, _config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        Ok(Box::new(InitialGuidanceProvider {
            role,
            requests: Arc::clone(&self.0),
        }))
    }
}

#[async_trait]
impl Provider for InitialGuidanceProvider {
    fn name(&self) -> &str {
        "application-task-test"
    }
    fn capabilities(&self) -> ProviderCapabilities {
        application_task_provider_capabilities()
    }
    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        if self.role == AgentRole::Planner {
            self.requests
                .lock()
                .expect("planner request capture")
                .push(request.clone());
        }
        ApplicationTaskRoleProvider { role: self.role }
            .stream(request)
            .await
    }
}

#[tokio::test]
async fn application_unplanned_continuation_delivers_guidance_through_the_real_entrypoint()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let config_path = root.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let mut config = RootConfig::load(&config_path)?;
    config.composition = sigil_kernel::RuntimeCompositionConfig::core();
    config
        .composition
        .enhancements
        .insert(sigil_kernel::OptionalCapability::TaskOrchestration);
    config.permission.mode = sigil_kernel::PermissionMode::DangerFullAccess;
    config.memory = MemoryConfig::with_enabled(false);
    config.verification.checks.clear();
    std::fs::write(&config_path, config.persisted_toml()?)?;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter)),
        &config_path,
        root.path(),
    )?
    .with_task_role_provider_builder(Arc::new(InitialGuidanceProviderBuilder(Arc::clone(
        &requests,
    ))));
    let mut prepared = Box::pin(prepare_application_run(
        ApplicationRunRequest::non_interactive(
            &config_path,
            root.path(),
            "Resume task planning",
            "initial-guidance-seed",
        ),
        &services,
    ))
    .await?;
    let session_path = prepared.session_log_path().to_path_buf();
    let session_scope = prepared.session_id().to_owned();
    let task_id = TaskId::new("application-initial-guidance")?;
    let objective = "Inspect the application task lifecycle";
    let guidance = "Prioritize the retry boundary and preserve the existing objective";
    prepared
        .execution
        .session
        .append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: prepared.execution.parent_session_ref.clone(),
            objective: objective.to_owned(),
            title: None,
            status: TaskRunStatus::Paused,
            reason: Some("planning paused before its first request".to_owned()),
        }))?;
    drop(prepared);
    let prepared = Box::pin(prepare_application_task_continuation(
        ApplicationTaskContinuationRequest {
            config_path,
            launch_cwd: root.path().to_path_buf(),
            session_path: session_path.clone(),
            session_attachment: None,
            expected_session_scope_id: session_scope,
            run_id: "application-initial-guidance-run".to_owned(),
            task_id: task_id.clone(),
            guidance: Some(guidance.to_owned()),
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    ))
    .await?;
    let (execution, control) = prepared.into_parts();
    let output = Box::pin(execution.execute(
        &mut RecordingApplicationRunEvents::default(),
        &mut AutoApproveHandler,
    ))
    .await?;
    drop(control);
    assert_eq!(output.task_status, TaskRunStatus::Completed);
    assert!(
        requests
            .lock()
            .expect("planner request capture")
            .iter()
            .any(|request| {
                request
                    .tools
                    .iter()
                    .any(|tool| tool.name == TASK_PLAN_UPDATE_TOOL_NAME)
                    && request.messages.iter().any(|message| {
                        message
                            .content
                            .as_deref()
                            .is_some_and(|text| text.contains(guidance) && text.contains(objective))
                    })
            })
    );
    let store = JsonlSessionStore::new(&session_path)?;
    let session =
        Session::load_from_store("application-task-test", "application-task-model", store)?;
    assert_eq!(
        session.task_state_projection().tasks[&task_id].objective,
        objective
    );
    let selected = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(selection))
                if selection.task_id == task_id =>
            {
                Some(selection)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(selected.len(), 1);
    assert!(selected[0].plan_version.is_none());
    assert_eq!(selected[0].guidance, guidance);
    Ok(())
}
