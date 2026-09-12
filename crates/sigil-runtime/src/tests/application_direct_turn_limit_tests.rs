//! Direct execution keeps working by default and respects only the configured turn limit.
use super::*;

struct DirectExecutionBuilder(Arc<AtomicUsize>);
struct DirectExecutionProvider(Arc<AtomicUsize>);

#[async_trait]
impl TaskRoleProviderBuilder for DirectExecutionBuilder {
    async fn build(&self, _config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        if role == AgentRole::Executor {
            Ok(Box::new(DirectExecutionProvider(Arc::clone(&self.0))))
        } else {
            Ok(Box::new(ApplicationTaskRoleProvider { role }))
        }
    }
}

#[async_trait]
impl Provider for DirectExecutionProvider {
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
        let stage = self.0.fetch_add(1, Ordering::SeqCst);
        let (name, args) = match stage {
            0 => ("bash", serde_json::json!({"command": "false"})),
            1..=8 => ("read_file", serde_json::json!({"path": "observation.txt"})),
            9 => ("bash", serde_json::json!({"command": "true"})),
            10 => {
                return Ok(Box::pin(stream::iter(scripted_task_completion_chunks(
                    &request,
                    "Inspection and follow-up check complete",
                    "direct-execution-completion",
                ))));
            }
            11 => {
                return Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderChunk::TextDelta(
                        "Inspection and follow-up check complete".to_owned(),
                    )),
                    Ok(ProviderChunk::Done),
                ])));
            }
            _ => anyhow::bail!("unexpected direct execution request {stage}"),
        };
        let id = format!("direct-execution-{stage}");
        let args_json = args.to_string();
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: id.clone(),
                name: name.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: id.clone(),
                delta: args_json.clone(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id,
                name: name.to_owned(),
                args_json,
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[tokio::test]
async fn direct_execution_continues_repeated_inspection_after_failure_by_default() -> Result<()> {
    assert_direct_execution_turn_limit(false).await
}

#[tokio::test]
async fn configured_direct_execution_turn_limit_pauses_and_resumes_after_restart() -> Result<()> {
    assert_direct_execution_turn_limit(true).await
}

async fn assert_direct_execution_turn_limit(configured_limit: bool) -> Result<()> {
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
    assert_eq!(
        config.agent.max_turns, None,
        "the default has no turn limit"
    );
    if configured_limit {
        config.agent.max_turns = Some(4);
    }
    std::fs::write(&config_path, config.persisted_toml()?)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter)),
        &config_path,
        root.path(),
    )?
    .with_task_role_provider_builder(Arc::new(DirectExecutionBuilder(Arc::clone(&calls))));
    let mut prepared = Box::pin(prepare_application_run(
        ApplicationRunRequest::non_interactive(
            &config_path,
            root.path(),
            "Recover the stopped task",
            "direct-execution-seed",
        ),
        &services,
    ))
    .await?;
    let session_path = prepared.session_log_path().to_path_buf();
    let scope = prepared.session_id().to_owned();
    let task_id = TaskId::new("direct-execution-task")?;
    let objective = "Inspect the requested files and finish";
    let parent_session_ref = prepared.execution.parent_session_ref.clone();
    prepared.execution.session.append_controls(vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref,
            objective: objective.to_owned(),
            title: None,
            status: TaskRunStatus::Paused,
            reason: Some("approved Direct Task ready".to_owned()),
        }),
        ControlEntry::TaskDirectExecutionAdmittedV1(
            sigil_kernel::TaskDirectExecutionAdmittedV1::approved_plan(
                task_id.clone(),
                objective,
                sigil_kernel::PlanId::new("direct-execution-plan")?,
                format!("sha256:{}", "c".repeat(64)),
                1,
            ),
        ),
    ])?;
    drop(prepared);
    let continuation = |run: &str| ApplicationTaskContinuationRequest {
        config_path: config_path.clone(),
        launch_cwd: root.path().to_path_buf(),
        session_path: session_path.clone(),
        session_attachment: None,
        expected_session_scope_id: scope.clone(),
        run_id: run.to_owned(),
        task_id: task_id.clone(),
        guidance: None,
        interaction: ApplicationRunInteraction::NonInteractive,
        permission_mode: None,
    };
    std::fs::write(
        root.path().join("observation.txt"),
        "The requested observation",
    )?;
    let first = Box::pin(prepare_application_task_continuation(
        continuation("direct-execution-first"),
        &services,
    ))
    .await?;
    let (execution, control) = first.into_parts();
    let output = Box::pin(execution.execute(
        &mut RecordingApplicationRunEvents::default(),
        &mut AutoApproveHandler,
    ))
    .await?;
    drop(control);
    if configured_limit {
        assert_eq!(output.task_status, TaskRunStatus::Paused);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "the explicit max_turns must stop before a fifth provider request"
        );
        let entries = JsonlSessionStore::read_entries(&session_path)?;
        assert_eq!(
            entries
                .iter()
                .filter(|entry| matches!(entry, SessionLogEntry::ToolResultV3(_)))
                .count(),
            4,
            "all four executed tools must be durable before restart"
        );
        drop(services);
        config.agent.max_turns = None;
        std::fs::write(&config_path, config.persisted_toml()?)?;
        let services = crate::r71_authority_composition::attach_boot_authority_to_services(
            ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter)),
            &config_path,
            root.path(),
        )?
        .with_task_role_provider_builder(Arc::new(DirectExecutionBuilder(Arc::clone(&calls))));
        let resumed = Box::pin(prepare_application_task_continuation(
            continuation("direct-execution-resumed"),
            &services,
        ))
        .await?;
        let (execution, control) = resumed.into_parts();
        let output = Box::pin(execution.execute(
            &mut RecordingApplicationRunEvents::default(),
            &mut AutoApproveHandler,
        ))
        .await?;
        assert_eq!(output.task_status, TaskRunStatus::Completed);
        drop(control);
    } else {
        assert_eq!(output.task_status, TaskRunStatus::Completed);
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        12,
        "the completion claim adds one final provider turn after the ten tool turns"
    );
    let entries = JsonlSessionStore::read_entries(&session_path)?;
    let results: Vec<_> = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::ToolResultV3(result) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 11);
    assert!(results[0].facts.error.is_some());
    assert!(
        results[1..]
            .iter()
            .all(|result| result.facts.error.is_none())
    );
    assert_eq!(
        Session::load_from_store(
            "application-task-test",
            "gpt-test",
            JsonlSessionStore::new(session_path)?,
        )?
        .task_state_projection()
        .tasks[&task_id]
            .status,
        TaskRunStatus::Completed
    );
    assert!(
        !entries.iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(_))
        )),
        "plain continuation must not require guidance to reset an implicit budget"
    );
    Ok(())
}
