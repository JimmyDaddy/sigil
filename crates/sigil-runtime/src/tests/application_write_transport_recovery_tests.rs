//! A settled managed mutation survives an ambiguous provider transport failure and restart.
use super::*;

const WRITE_CALL: &str = "transport-recovery-write";
const FILE_TEXT: &str = "one durable managed mutation\n";
const FINAL_TEXT: &str = "The existing file mutation is complete.";

struct WriteTransportBuilder(Arc<AtomicUsize>);
struct WriteTransportProvider(Arc<AtomicUsize>);

#[async_trait]
impl TaskRoleProviderBuilder for WriteTransportBuilder {
    async fn build(&self, _config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        if role == AgentRole::Executor {
            Ok(Box::new(WriteTransportProvider(Arc::clone(&self.0))))
        } else {
            Ok(Box::new(ApplicationTaskRoleProvider { role }))
        }
    }
}

#[async_trait]
impl Provider for WriteTransportProvider {
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
        match self.0.fetch_add(1, Ordering::SeqCst) {
            0 => {
                let args_json = serde_json::json!({
                    "path": "settled.txt", "content": FILE_TEXT
                })
                .to_string();
                Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderChunk::ToolCallStart {
                        id: WRITE_CALL.to_owned(),
                        name: "write_file".to_owned(),
                    }),
                    Ok(ProviderChunk::ToolCallArgsDelta {
                        id: WRITE_CALL.to_owned(),
                        delta: args_json.clone(),
                    }),
                    Ok(ProviderChunk::ToolCallComplete(ToolCall {
                        id: WRITE_CALL.to_owned(),
                        name: "write_file".to_owned(),
                        args_json,
                    })),
                    Ok(ProviderChunk::Done),
                ])))
            }
            stage @ (1 | 2) => {
                let results = request
                    .messages
                    .iter()
                    .filter(|message| {
                        message.tool_call_id.as_deref() == Some(WRITE_CALL)
                            && message.tool_result_payload.is_some()
                    })
                    .count();
                anyhow::ensure!(
                    results == 1,
                    "settled V3 tool fact must occur once in request"
                );
                if stage == 1 {
                    // Deliberately unclassified: there is no safe automatic resend proof.
                    anyhow::bail!("unclassified transport interruption after settled write");
                }
                Ok(Box::pin(stream::iter(scripted_task_completion_chunks(
                    &request,
                    FINAL_TEXT,
                    "transport-recovery-completion",
                ))))
            }
            3 => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta(FINAL_TEXT.to_owned())),
                Ok(ProviderChunk::Done),
            ]))),
            stage => anyhow::bail!("unexpected provider resend at stage {stage}"),
        }
    }
}

#[tokio::test]
async fn direct_task_write_then_transport_failure_restarts_without_repeating_mutation() -> Result<()>
{
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
    config.agent.max_turns = Some(8);
    std::fs::write(&config_path, config.persisted_toml()?)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter)),
        &config_path,
        root.path(),
    )?
    .with_task_role_provider_builder(Arc::new(WriteTransportBuilder(Arc::clone(&calls))));
    let mut prepared = Box::pin(prepare_application_run(
        ApplicationRunRequest::non_interactive(
            &config_path,
            root.path(),
            "Write the requested file once",
            "write-transport-seed",
        ),
        &services,
    ))
    .await?;
    let session_path = prepared.session_log_path().to_path_buf();
    let scope = prepared.session_id().to_owned();
    let task_id = TaskId::new("write-transport-task")?;
    let objective = "Write settled.txt once, then report the completed mutation";
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
                sigil_kernel::PlanId::new("write-transport-plan")?,
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
    let first = Box::pin(prepare_application_task_continuation(
        continuation("write-before-transport-failure"),
        &services,
    ))
    .await?;
    let (execution, control) = first.into_parts();
    let output = Box::pin(execution.execute(
        &mut RecordingApplicationRunEvents::default(),
        &mut AutoApproveHandler,
    ))
    .await?;
    assert_eq!(output.task_status, TaskRunStatus::Paused);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "ambiguous transport must not automatically resend"
    );
    drop(control);
    assert_eq!(
        std::fs::read_to_string(root.path().join("settled.txt"))?,
        FILE_TEXT
    );
    let modified = std::fs::metadata(root.path().join("settled.txt"))?.modified()?;
    let entries = JsonlSessionStore::read_entries(&session_path)?;
    let write = entries
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::ToolResultV3(result) if result.call_id == WRITE_CALL => Some(result),
            _ => None,
        })
        .expect("physical write must settle before provider interruption");
    assert!(write.facts.error.is_none());
    assert_eq!(write.facts.status, "ok");
    assert!(!write.facts.changed_files.is_empty());
    let settled_message_id = write.message_id.clone();
    assert!(!entries.iter().any(|entry| matches!(entry,
        SessionLogEntry::Assistant(message) if message.content.as_deref() == Some(FINAL_TEXT)
    )));
    drop(entries);
    drop(services);

    // Rebuild boot services and reload the same durable Task through its public continuation.
    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter)),
        &config_path,
        root.path(),
    )?
    .with_task_role_provider_builder(Arc::new(WriteTransportBuilder(Arc::clone(&calls))));
    let resumed = Box::pin(prepare_application_task_continuation(
        continuation("write-after-restart"),
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
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        std::fs::read_to_string(root.path().join("settled.txt"))?,
        FILE_TEXT
    );
    assert_eq!(
        std::fs::metadata(root.path().join("settled.txt"))?.modified()?,
        modified,
        "recovery must not perform another disk write"
    );
    let entries = JsonlSessionStore::read_entries(&session_path)?;
    let writes: Vec<_> = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::ToolResultV3(result) if result.tool_name == "write_file" => {
                Some(result)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        writes.len(),
        1,
        "exactly one physical write result must exist"
    );
    assert_eq!(writes[0].message_id, settled_message_id);
    assert_eq!(
        entries
            .iter()
            .filter_map(|entry| match entry {
                SessionLogEntry::Assistant(message) => Some(message),
                _ => None,
            })
            .flat_map(|message| &message.tool_calls)
            .filter(|call| call.name == "write_file")
            .count(),
        1
    );
    assert_eq!(entries.iter().filter(|entry| matches!(entry,
        SessionLogEntry::Assistant(message) if message.content.as_deref() == Some(FINAL_TEXT)
    )).count(), 1, "the resumed Task must publish its final answer once");
    let recovered = Session::load_from_store(
        "application-task-test",
        "gpt-test",
        JsonlSessionStore::new(&session_path)?,
    )?;
    let projection = recovered.task_state_projection();
    assert_eq!(projection.tasks.len(), 1);
    assert_eq!(
        projection
            .tasks
            .get(&task_id)
            .expect("same durable Task")
            .status,
        TaskRunStatus::Completed
    );
    Ok(())
}
