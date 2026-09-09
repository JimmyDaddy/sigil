//! Continuations retain the shipping authority, policy, broker, registry and physical file tools.
//! Only provider responses are scripted so the regression needs no network or credentials.

use super::*;
use crate::application_run::{
    current_schema_managed_artifact_store_writer, current_schema_managed_session_log_writer,
    current_schema_tool_authority, prepare_application_run_blocking_with_writer,
};

const WRITTEN_TEXT: &str = "written after the durable answer";
const TASK_TEXT: &str = "read by the continued task";

#[test]
fn blocking_prepare_rejects_missing_boot_composition_before_namespace_admission() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (config_path, services) = continuation_services(root.path(), false)?;
    let managed_path = services
        .authority_composition()
        .expect("real boot composition")
        .storage_writer
        .session_log_path_for_key("missing-selection")?;
    let requested_path = root.path().join("new-sessions/missing-selection.jsonl");
    assert!(!managed_path.exists());
    assert!(!requested_path.parent().expect("requested parent").exists());
    let mut request = ApplicationRunRequest::non_interactive(
        &config_path,
        root.path(),
        "prepare without an expected boot selection",
        "missing-selection-run",
    );
    request.session_path = Some(requested_path.clone());
    let error = match prepare_application_run_blocking_with_writer(
        request,
        Arc::clone(&services.session_leases),
        false,
        current_schema_tool_authority(&services),
        None,
        current_schema_managed_session_log_writer(&services),
        current_schema_managed_artifact_store_writer(&services),
    ) {
        Ok(_) => anyhow::bail!("missing boot composition must be rejected in every build"),
        Err(error) => error,
    };
    assert_eq!(
        error.class(),
        ApplicationRunPrepareErrorClass::AuthorityUnavailable
    );
    assert!(!managed_path.exists());
    assert!(!requested_path.parent().expect("requested parent").exists());
    Ok(())
}

#[tokio::test]
async fn fresh_session_rejects_config_selection_drift_before_namespace_admission() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (config_path, services) = continuation_services(root.path(), false)?;
    let original = RootConfig::load(&config_path)?;
    let managed_path = services
        .authority_composition()
        .expect("real boot composition")
        .storage_writer
        .session_log_path_for_key("selection-guard")?;
    assert!(!managed_path.exists());
    let requested_path = root.path().join("new-sessions/selection-guard.jsonl");
    let mut changed = original.clone();
    changed
        .composition
        .enhancements
        .insert(sigil_kernel::OptionalCapability::Terminal);
    std::fs::write(&config_path, changed.persisted_toml()?)?;
    let request = || {
        let mut request = ApplicationRunRequest::non_interactive(
            &config_path,
            root.path(),
            "prepare a fresh session",
            "selection-guard-run",
        );
        request.session_path = Some(requested_path.clone());
        request
    };
    let error = match Box::pin(prepare_application_run(request(), &services)).await {
        Ok(_) => anyhow::bail!("a new session cannot admit capabilities absent from the boot"),
        Err(error) => error,
    };
    let ApplicationRunPrepareError::Configuration { source } = error else {
        anyhow::bail!("composition drift must be a configuration error: {error:?}");
    };
    assert!(
        source.to_string().contains("restart"),
        "configuration failure must identify the required restart: {source:#}"
    );
    assert!(
        !managed_path.exists(),
        "mismatch must not admit a managed namespace"
    );
    assert!(!requested_path.parent().expect("requested parent").exists());
    // A non-composition config edit remains legal for a fresh session under the same boot.
    let mut same_selection = original;
    same_selection.agent.max_turns = Some(3);
    std::fs::write(&config_path, same_selection.persisted_toml()?)?;
    let prepared = Box::pin(prepare_application_run(request(), &services)).await?;
    assert!(prepared.terminal_control().is_none());
    assert_eq!(prepared.run_options().max_turns, Some(3));
    assert_eq!(
        prepared.session_log_path(),
        managed_path.join("records.jsonl")
    );
    Ok(())
}

fn continuation_services(
    root: &Path,
    task: bool,
) -> Result<(std::path::PathBuf, ApplicationRunServices)> {
    let config_path = root.join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let mut config = RootConfig::load(&config_path)?;
    config.composition = sigil_kernel::RuntimeCompositionConfig::core();
    if task {
        config
            .composition
            .enhancements
            .insert(sigil_kernel::OptionalCapability::TaskOrchestration);
    }
    config.permission.mode = sigil_kernel::PermissionMode::DangerFullAccess;
    std::fs::write(&config_path, config.persisted_toml()?)?;
    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter)),
        &config_path,
        root,
    )?;
    services.require_current_schema_authority()?;
    Ok((config_path, services))
}

fn tool_chunks(id: &str, name: &str, args: serde_json::Value) -> Vec<Result<ProviderChunk>> {
    let args_json = args.to_string();
    vec![
        Ok(ProviderChunk::ToolCallStart {
            id: id.to_owned(),
            name: name.to_owned(),
        }),
        Ok(ProviderChunk::ToolCallArgsDelta {
            id: id.to_owned(),
            delta: args_json.clone(),
        }),
        Ok(ProviderChunk::ToolCallComplete(ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            args_json,
        })),
        Ok(ProviderChunk::Done),
    ]
}

fn request_contains(request: &CompletionRequest, expected: &str) -> bool {
    request.messages.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains(expected))
    })
}

struct QuestionThenFileProvider {
    stage: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for QuestionThenFileProvider {
    fn name(&self) -> &str {
        "custom"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        application_task_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let chunks = match self.stage.fetch_add(1, Ordering::SeqCst) {
            0 => tool_chunks(
                "continuation-question",
                sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME,
                serde_json::json!({
                    "prompt": "Choose a mode before writing",
                    "questions": [{
                        "id": "mode", "header": "Mode", "question": "Which mode?",
                        "required": true,
                        "field": {"kind": "text", "multiline": false, "max_chars": 32}
                    }]
                }),
            ),
            1 => {
                anyhow::ensure!(
                    request_contains(&request, "approved-mode"),
                    "answer was not included"
                );
                tool_chunks(
                    "continuation-write",
                    "write_file",
                    serde_json::json!({"path": "answer.txt", "content": WRITTEN_TEXT}),
                )
            }
            2 => tool_chunks(
                "continuation-read",
                "read_file",
                serde_json::json!({"path": "answer.txt"}),
            ),
            3 => {
                anyhow::ensure!(
                    request_contains(&request, WRITTEN_TEXT),
                    "file tool did not return the written data"
                );
                vec![
                    Ok(ProviderChunk::TextDelta(
                        "continued file work complete".to_owned(),
                    )),
                    Ok(ProviderChunk::Done),
                ]
            }
            _ => anyhow::bail!("unexpected continuation provider turn"),
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

fn install_question_provider(
    prepared: &mut super::super::PreparedApplicationRun,
    stage: &Arc<AtomicUsize>,
) {
    let ApplicationRunExecutionKind::Main { agent, .. } = &mut prepared.execution.kind else {
        panic!("Core must use the ordinary agent loop");
    };
    let registry = agent.tool_registry().clone();
    **agent = sigil_kernel::Agent::new(
        Box::new(QuestionThenFileProvider {
            stage: Arc::clone(stage),
        }) as Box<dyn Provider>,
        registry,
    );
}

#[tokio::test]
async fn submitted_answer_continuation_executes_real_managed_write_and_read() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (config_path, services) = continuation_services(root.path(), false)?;
    let stage = Arc::new(AtomicUsize::new(0));
    let mut prepared = Box::pin(prepare_application_run(
        ApplicationRunRequest::non_interactive(
            &config_path,
            root.path(),
            "ask then write",
            "question-before-file-work",
        ),
        &services,
    ))
    .await?;
    let session_path = prepared.session_log_path().to_path_buf();
    let scope = prepared.session_id().to_owned();
    install_question_provider(&mut prepared, &stage);
    let (execution, control) = prepared.into_parts();
    let output = Box::pin(execution.execute(
        &mut RecordingApplicationRunEvents::default(),
        &mut AutoApproveHandler,
    ))
    .await?;
    let AgentRunDisposition::AwaitingUserInput(pending) = output.agent_output.disposition else {
        anyhow::bail!("the real request_user_input tool must suspend the first run");
    };
    drop(control);
    assert!(!root.path().join("answer.txt").exists());
    let decision = Box::pin(prepare_application_user_input_decision(
        ApplicationUserInputDecisionRequest {
            config_path,
            launch_cwd: root.path().to_path_buf(),
            session_path: session_path.clone(),
            session_attachment: None,
            expected_session_scope_id: scope,
            run_id: "file-work-after-answer".to_owned(),
            identity: pending.identity,
            request_hash: pending.request_hash,
            command_id: UserInputCommandId::new("submit-mode-for-files")?,
            decision: UserInputDecisionV1::Submitted {
                answers: vec![UserInputAnswerV1 {
                    question_id: "mode".to_owned(),
                    value: UserInputAnswerValueV1::Text {
                        value: "approved-mode".to_owned(),
                    },
                }],
            },
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    ))
    .await?;
    let (_, continuation, revision) = decision.into_parts();
    assert!(revision.is_none());
    let mut continuation = continuation.expect("submitted answer continuation");
    install_question_provider(&mut continuation, &stage);
    let (execution, control) = continuation.into_parts();
    let output = Box::pin(execution.execute(
        &mut RecordingApplicationRunEvents::default(),
        &mut AutoApproveHandler,
    ))
    .await?;
    assert_eq!(
        output.terminal_status,
        ApplicationRunTerminalStatus::Succeeded
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("answer.txt"))?,
        WRITTEN_TEXT
    );
    drop(control);
    let entries = JsonlSessionStore::read_entries(&session_path)?;
    for call_id in ["continuation-write", "continuation-read"] {
        let result = entries
            .iter()
            .find_map(|entry| match entry {
                SessionLogEntry::ToolResultV3(result) if result.call_id == call_id => Some(result),
                _ => None,
            })
            .expect("real file result must be durable");
        assert!(
            result.facts.error.is_none(),
            "{call_id}: {:?}",
            result.facts.error
        );
    }
    assert_eq!(stage.load(Ordering::SeqCst), 4);
    Ok(())
}

struct ReadingTaskProviderBuilder {
    executor_calls: Arc<AtomicUsize>,
}

struct ReadingTaskProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl TaskRoleProviderBuilder for ReadingTaskProviderBuilder {
    async fn build(&self, _config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        if role == AgentRole::Executor {
            Ok(Box::new(ReadingTaskProvider {
                calls: Arc::clone(&self.executor_calls),
            }))
        } else {
            Ok(Box::new(ApplicationTaskRoleProvider { role }))
        }
    }
}

#[async_trait]
impl Provider for ReadingTaskProvider {
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
        let chunks = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => tool_chunks(
                "continued-task-read",
                "read_file",
                serde_json::json!({"path": "task.txt"}),
            ),
            1 => {
                anyhow::ensure!(
                    request_contains(&request, TASK_TEXT),
                    "continued Task lost file authority"
                );
                vec![
                    Ok(ProviderChunk::TextDelta(
                        "task file read complete".to_owned(),
                    )),
                    Ok(ProviderChunk::Done),
                ]
            }
            _ => anyhow::bail!("unexpected Task executor turn"),
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

#[tokio::test]
async fn task_continuation_executes_real_managed_file_read() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (config_path, services) = continuation_services(root.path(), true)?;
    std::fs::write(root.path().join("task.txt"), TASK_TEXT)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let services = services.with_task_role_provider_builder(Arc::new(ReadingTaskProviderBuilder {
        executor_calls: Arc::clone(&calls),
    }));
    let mut prepared = Box::pin(prepare_application_run(
        ApplicationRunRequest::non_interactive(
            &config_path,
            root.path(),
            "prepare task",
            "task-seed",
        ),
        &services,
    ))
    .await?;
    let session_path = prepared.session_log_path().to_path_buf();
    let scope = prepared.session_id().to_owned();
    let task_id = TaskId::new("continued-file-task")?;
    let parent_session_ref = prepared.execution.parent_session_ref.clone();
    prepared
        .execution
        .session
        .append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref,
            objective: "read the task fixture".to_owned(),
            title: None,
            status: TaskRunStatus::Paused,
            reason: Some("restart before the first task step".to_owned()),
        }))?;
    drop(prepared);
    let continued = Box::pin(prepare_application_task_continuation(
        ApplicationTaskContinuationRequest {
            config_path,
            launch_cwd: root.path().to_path_buf(),
            session_path,
            session_attachment: None,
            expected_session_scope_id: scope,
            run_id: "continued-task-file-run".to_owned(),
            task_id,
            guidance: None,
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    ))
    .await?;
    let (execution, control) = continued.into_parts();
    let output = Box::pin(execution.execute(
        &mut RecordingApplicationRunEvents::default(),
        &mut AutoApproveHandler,
    ))
    .await?;
    assert_eq!(output.task_status, TaskRunStatus::Completed);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the Task must consume one real read result"
    );
    drop(control);
    Ok(())
}
