//! Real HTTP command ownership, Direct execution and durable replay; only provider bytes are scripted.
use super::*;
use futures::{Stream, stream};
use sigil_kernel::{
    CompletionRequest, Provider, ProviderCapabilities, ProviderChunk, ReasoningStreamSupport,
};
use sigil_runtime::agent_supervisor::task_role_runtime::TaskRoleProviderBuilder;
use std::pin::Pin;

// A failed assertion must release the owned blocking execution before Tokio teardown.
struct DirectDeliveryReleaseGuard(Arc<tokio::sync::Semaphore>);
impl Drop for DirectDeliveryReleaseGuard {
    fn drop(&mut self) {
        self.0.close();
    }
}

struct DirectDeliveryProvider {
    calls: Arc<AtomicUsize>,
    entered: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
}

#[async_trait]
impl Provider for DirectDeliveryProvider {
    fn name(&self) -> &str {
        "http-direct-fixture"
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: true,
            reports_cache_tokens: true,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: true,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: true,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: true,
            tool_name_max_chars: 64,
        }
    }
    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        assert!(request.tools.iter().all(|tool| !matches!(
            tool.name.as_str(),
            "task_completion_claim" | "bind_direct_task_requirements"
        )));
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = match index {
            0 => {
                self.entered.add_permits(1);
                self.release.acquire().await?.forget();
                let args_json =
                    serde_json::json!({"path":"delivered.txt", "content":"written exactly once\n"})
                        .to_string();
                vec![
                    Ok(ProviderChunk::ToolCallStart {
                        id: "http-direct-write".to_owned(),
                        name: "write_file".to_owned(),
                    }),
                    Ok(ProviderChunk::ToolCallArgsDelta {
                        id: "http-direct-write".to_owned(),
                        delta: args_json.clone(),
                    }),
                    Ok(ProviderChunk::ToolCallComplete(ToolCall {
                        id: "http-direct-write".to_owned(),
                        name: "write_file".to_owned(),
                        args_json,
                    })),
                    Ok(ProviderChunk::Done),
                ]
            }
            1 => vec![
                Ok(ProviderChunk::TextDelta("Delivery confirmed.".to_owned())),
                Ok(ProviderChunk::Done),
            ],
            _ => anyhow::bail!("replay must not invoke provider again: {index}"),
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}
struct DirectDeliveryPreparer {
    calls: Arc<AtomicUsize>,
    entered: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
}
#[async_trait]
impl TaskRoleProviderBuilder for DirectDeliveryPreparer {
    async fn build(
        &self,
        _config: &sigil_kernel::RootConfig,
        _role: AgentRole,
    ) -> Result<Box<dyn Provider>> {
        Ok(Box::new(DirectDeliveryProvider {
            calls: Arc::clone(&self.calls),
            entered: Arc::clone(&self.entered),
            release: Arc::clone(&self.release),
        }))
    }
}
#[async_trait]
impl HttpApplicationRunPreparer for DirectDeliveryPreparer {
    async fn prepare(
        &self,
        request: ApplicationRunRequest,
        services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        Ok(prepare_application_run(request, &services).await?)
    }
    async fn prepare_queued(
        &self,
        request: ApplicationQueuedRunRequest,
        services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        Ok(prepare_application_queued_run(request, &services).await?)
    }
    async fn prepare_task(
        &self,
        request: ApplicationTaskContinuationRequest,
        services: ApplicationRunServices,
    ) -> Result<PreparedApplicationTaskContinuation> {
        let services = services.with_task_role_provider_builder(Arc::new(Self {
            calls: Arc::clone(&self.calls),
            entered: Arc::clone(&self.entered),
            release: Arc::clone(&self.release),
        }));
        Ok(prepare_application_task_continuation(request, &services).await?)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_direct_continuation_retry_and_delivery_replay_do_not_repeat_execution()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_production_test_config(&config_path, ".");
    let mut config = sigil_kernel::RootConfig::load(&config_path)?;
    config.composition = sigil_kernel::RuntimeCompositionConfig::core();
    config
        .composition
        .enhancements
        .insert(sigil_kernel::OptionalCapability::TaskOrchestration);
    config.permission.mode = sigil_kernel::PermissionMode::DangerFullAccess;
    std::fs::write(&config_path, config.persisted_toml()?)?;
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(
        32,
        Arc::new(HttpDurableProtocolJournal::open(
            temp.path().join("protocol.json"),
            64,
        )?),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let _release_on_exit = DirectDeliveryReleaseGuard(Arc::clone(&release));
    let driver = Arc::new(HttpProductionRunDriver::new_with_preparer(
        HttpProductionRunDriverOptions::new(&config_path, temp.path()).with_session_lifecycle(
            sigil_runtime::LocalSessionLifecycleService::new(
                "http-direct-delivery",
                temp.path().join("sessions"),
                temp.path().join("exports"),
            ),
        ),
        Arc::new(HttpDurableEgressDisclosureJournal::open(
            temp.path().join("disclosures.json"),
            32,
        )?),
        Arc::clone(&event_bus),
        tokio::runtime::Handle::current(),
        Arc::new(DirectDeliveryPreparer {
            calls: Arc::clone(&calls),
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        }),
    )?);
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        temp.path().join("commands.json"),
        32,
    )?))?;
    let create_registry = registry.clone();
    let session = tokio::task::spawn_blocking(move || {
        create_registry.create_session(HttpSessionCreateRequest::default())
    })
    .await??;
    let task_id = TaskId::new("http-direct-delivery")?;
    let objective = "Write the requested file once";
    let mut durable = sigil_kernel::Session::load_from_store(
        "custom",
        "gpt-test",
        JsonlSessionStore::new(&session.session_log_path)?,
    )?;
    let draft = sigil_kernel::plan_draft_created_entry(
        &format!("```sigil-plan-v2\n{}\n```", serde_json::json!({
            "summary":"Write the requested file once", "steps":[{
                "step_id":"write", "title":"Write the requested file", "role":"executor", "depends_on":[],
                "mode":"write", "isolation":"sequential_workspace_write", "deliverables":[objective]
            }], "target_paths":[]
        })), sigil_kernel::PlanSourceRef::default(), 1, None,
    )?.expect("approved artifact");
    let admission = sigil_kernel::TaskDirectExecutionAdmittedV1::approved_plan(
        task_id.clone(),
        objective,
        draft.plan_id.clone(),
        draft.plan_hash.clone(),
        1,
    );
    durable.append_control(ControlEntry::PlanDraftCreated(draft))?;
    durable.append_controls(vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: objective.to_owned(),
            title: None,
            status: TaskRunStatus::Paused,
            reason: None,
        }),
        ControlEntry::TaskDirectExecutionAdmittedV1(admission),
    ])?;
    drop(durable);
    let command = HttpCommandEnvelope::new(
        "continue-once",
        "desktop-client",
        &session.id,
        HttpRunStartRequest {
            prompt: String::new(),
            permission_mode: Some(HttpPermissionMode::DangerFullAccess),
            model_ref: None,
            model_selection_binding: None,
            route_recovery_binding: None,
            reasoning_effort: None,
            reasoning_effort_binding: None,
            skill_binding: None,
            agent_binding: None,
            task_continuation: Some(HttpTaskContinuationRequest {
                task_id: task_id.as_str().to_owned(),
                guidance: None,
            }),
        },
    );
    let submit = |command: HttpCommandEnvelope<HttpRunStartRequest>| {
        let registry = registry.clone();
        let id = session.id.clone();
        tokio::task::spawn_blocking(move || registry.start_run_command(&id, command))
    };
    // Disconnect the observer before completion; durable delivery must survive its missing ACK.
    drop(event_bus.subscribe());
    let first = submit(command.clone()).await??;
    tokio::time::timeout(Duration::from_secs(10), entered.acquire())
        .await??
        .forget();
    let replay = submit(command.clone()).await??;
    assert_eq!(first.run.id, replay.run.id);
    assert!(replay.replayed);
    let mut competing = command.clone();
    competing.command_id = "continue-other-surface".to_owned();
    competing.client_id = "tui-client".to_owned();
    assert!(
        submit(competing).await?.is_err(),
        "active Direct owner must reject another continuation"
    );
    let mut rewritten = command.clone();
    rewritten
        .payload
        .task_continuation
        .as_mut()
        .expect("typed continuation")
        .guidance = Some("rewritten old command".to_owned());
    assert!(
        matches!(
            submit(rewritten).await?,
            Err(HttpRegistryError::CommandKeyConflict { .. })
        ),
        "old command identity must retain the durable payload conflict"
    );
    release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if registry.get_run(&first.run.id)?.status.is_terminal() {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("delivered.txt"))?,
        "written exactly once\n"
    );
    let events =
        event_bus.replay_run_after(&session.durable_session_scope_id, &first.run.id, None)?;
    assert!(events.iter().any(|event| {
        event
            .run_event
            .as_ref()
            .is_some_and(|event| matches!(event.event, PublicRunEventKind::RunFinished { .. }))
    }));
    assert_eq!(
        registry.get_run(&first.run.id)?.status,
        HttpRunStatus::Finished
    );
    let catalog = driver
        .session_lifecycle()
        .expect("production lifecycle")
        .catalog()?;
    let source = catalog
        .entries
        .iter()
        .find(|entry| entry.session_id.as_deref() == Some(&session.durable_session_scope_id))
        .expect("managed catalog entry");
    let reopen = HttpSessionOpenRequest {
        session_ref: source.session_ref.as_path().to_string_lossy().into_owned(),
        session_id: session.durable_session_scope_id.clone(),
        label: None,
        recovery_binding: None,
    };
    let reopen_registry = registry.clone();
    tokio::task::spawn_blocking(move || reopen_registry.open_session(reopen)).await??;
    let old = submit(command).await??;
    assert_eq!(old.run.id, first.run.id);
    assert!(old.replayed);
    let durable = sigil_kernel::Session::load_from_store(
        "custom",
        "gpt-test",
        JsonlSessionStore::new(&session.session_log_path)?,
    )?;
    let projection = durable.task_state_projection();
    let task = &projection.tasks[&task_id];
    assert_eq!(task.status, TaskRunStatus::Completed);
    assert_eq!(task.direct_execution_attempts.len(), 1);
    assert_eq!(durable.entries().iter().filter(|entry| matches!(entry, SessionLogEntry::ToolResultV3(result) if result.call_id == "http-direct-write")).count(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    Ok(())
}
