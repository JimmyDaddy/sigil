use super::*;

#[derive(Clone, Default)]
struct TwoQuestionProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for TwoQuestionProvider {
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
        let mut requests = self.requests.lock().expect("fixture requests");
        let turn = requests.len();
        requests.push(request);
        let chunks = match turn {
            0 | 1 => vec![
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: format!("sequential-question-{}", turn + 1),
                    name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
                    args_json: serde_json::json!({"questions":[{"id":"scope",
                    "question":format!("Question {}?", turn + 1)}]})
                    .to_string(),
                })),
                Ok(ProviderChunk::Done),
            ],
            2 => vec![
                Ok(ProviderChunk::TextDelta(
                    "Both answers consumed exactly once.".to_owned(),
                )),
                Ok(ProviderChunk::Done),
            ],
            _ => anyhow::bail!(
                "sequential fixture received an unexpected additional provider request"
            ),
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

fn use_question_provider(
    execution: &mut super::super::ApplicationRunExecution,
    provider: &TwoQuestionProvider,
) -> Result<()> {
    let registry = execution.extension_registry.clone();
    let ApplicationRunExecutionKind::Main { agent, .. } = &mut execution.kind else {
        anyhow::bail!("ordinary conversation expected");
    };
    **agent = sigil_kernel::Agent::new(Box::new(provider.clone()), registry);
    Ok(())
}

#[test]
fn sequential_user_input_application_continuations_publish_two_real_questions() -> Result<()> {
    let test = std::thread::Builder::new()
        .name("user-input-continuation-small-stack".to_owned())
        .stack_size(1024 * 1024)
        .spawn(|| -> Result<()> {
            // Construct and poll the full prepare + execute + user-input continuation chain on a
            // Windows-sized 1 MiB stack so large async state machines or construction temporaries
            // fail immediately in local verification.
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(sequential_user_input_application_continuations_on_small_stack())
        })?;
    match test.join() {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[allow(clippy::await_holding_lock)]
async fn sequential_user_input_application_continuations_on_small_stack() -> Result<()> {
    let _environment = crate::test_env::lock();
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let services = with_application_test_managed_authority(
        temp.path(),
        ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter)),
    )?;
    let binding = bind_application_test_managed_session(
        &config_path,
        temp.path(),
        &temp.path().join("state/sessions/sequential-input.jsonl"),
        &services,
    )?;
    let mut request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "Clarify two missing decisions, then finish.",
        "sequential-root",
    );
    request.session_path = Some(binding.session_log_path.clone());
    let prepared = prepare_application_run(request, &services).await?;
    let provider = TwoQuestionProvider::default();
    let mut events = RecordingApplicationRunEvents::default();
    let (mut execution, control) = prepared.into_parts();
    use_question_provider(&mut execution, &provider)?;
    let mut output = execution
        .execute(&mut events, &mut AutoApproveHandler)
        .await?;
    drop(control);
    let mut references = Vec::new();
    for turn in 1..=2 {
        let AgentRunDisposition::AwaitingUserInput(reference) = &output.agent_output.disposition
        else {
            anyhow::bail!(
                "question {turn} was not durably published: {:?}",
                output.agent_output.disposition
            );
        };
        let reference = reference.clone();
        references.push(reference.clone());
        let prepared = prepare_application_user_input_decision(
            ApplicationUserInputDecisionRequest {
                application_operation: None,
                config_path: config_path.clone(),
                launch_cwd: temp.path().to_path_buf(),
                session_path: binding.session_log_path.clone(),
                session_attachment: None,
                expected_session_scope_id: binding.session_scope_id.clone(),
                run_id: format!("sequential-continuation-{turn}"),
                identity: reference.identity,
                request_hash: reference.request_hash,
                command_id: UserInputCommandId::new(format!("answer-{turn}"))?,
                decision: UserInputDecisionV1::Submitted {
                    answers: vec![UserInputAnswerV1 {
                        question_id: "scope".to_owned(),
                        value: UserInputAnswerValueV1::Text {
                            value: format!("answer {turn}"),
                        },
                    }],
                },
                interaction: ApplicationRunInteraction::NonInteractive,
                permission_mode: None,
            },
            &services,
        )
        .await?;
        let (_, continuation, revision) = prepared.into_parts();
        assert!(revision.is_none());
        let (mut execution, control) = continuation
            .expect("submitted answer continuation")
            .into_parts();
        use_question_provider(&mut execution, &provider)?;
        output = execution
            .execute(&mut events, &mut AutoApproveHandler)
            .await?;
        drop(control);
        let reopened = Session::load_from_store(
            "custom",
            "gpt-test",
            JsonlSessionStore::open_existing(&binding.session_log_path)?,
        )?;
        let projection = reopened.user_input_projection()?;
        assert_eq!(projection.pending().count(), usize::from(turn == 1));
        assert!(
            sigil_kernel::recoverable_user_input_decision(&reopened)?.is_none(),
            "completed answers must not become replay candidates after restart"
        );
    }
    assert_eq!(
        output.terminal_status,
        ApplicationRunTerminalStatus::Succeeded
    );
    assert_eq!(
        references[0].identity.root_logical_run_id,
        references[1].identity.root_logical_run_id
    );
    let requests = provider.requests.lock().expect("requests");
    assert_eq!(requests.len(), 3);
    for (index, call_id) in ["sequential-question-1", "sequential-question-2"]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            requests[index + 1]
                .messages
                .iter()
                .filter(|message| message.tool_call_id.as_deref() == Some(call_id))
                .count(),
            1
        );
    }
    let records = JsonlSessionStore::read_event_records(&binding.session_log_path)?;
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    for reference in &references {
        assert_eq!(outbox.events_in_order().iter().filter(|entry| matches!(&entry.event.event,
            PublicRunEventKind::UserInputChanged { request_id, status: UserInputStatusV1::Resolved, .. }
                if request_id == reference.identity.request_id.as_str())).count(), 1,
            "the already-consumed final settlement must be read-only, including public output");
    }
    Ok(())
}

#[tokio::test]
async fn user_input_publication_failure_aborts_instead_of_becoming_invalid_model_input()
-> Result<()> {
    struct RejectPublication;
    impl EventHandler for RejectPublication {
        fn handle(&mut self, _event: RunEvent) -> Result<()> {
            Ok(())
        }
        fn commit_controls(
            &mut self,
            session: &mut Session,
            controls: Vec<ControlEntry>,
        ) -> Result<Vec<sigil_kernel::StoredEvent>> {
            if controls
                .iter()
                .any(|control| matches!(control, ControlEntry::UserInputRequested(_)))
            {
                return Err(super::super::ApplicationPublicOutboxAppendError {
                    source: anyhow::anyhow!("injected durable publication rejection"),
                }
                .into());
            }
            session.append_controls_with_events(controls)
        }
    }
    let temp = tempfile::tempdir()?;
    let mut session = Session::load_from_store(
        "custom",
        "gpt-test",
        JsonlSessionStore::new(temp.path().join("publication-failure.jsonl"))?,
    )?;
    let provider = TwoQuestionProvider::default();
    let agent = sigil_kernel::Agent::new(provider.clone(), ToolRegistry::new());
    let error = agent
        .run_with_input(
            &mut session,
            sigil_kernel::AgentRunInput::user("ask one question")
                .with_logical_run_id("publication-root"),
            sigil_kernel::AgentRunOptions {
                workspace_root: temp.path().to_path_buf(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Headless,
                permission_config: sigil_kernel::PermissionConfig::default(),
                permission_context: sigil_kernel::PermissionEvaluationContext::default(),
                permission_mode_override: None,
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: sigil_kernel::CompactionConfig::default(),
                tool_authority: None,
            },
            &mut RejectPublication,
        )
        .await
        .expect_err("publication failure must abort the run");
    assert!(format!("{error:#}").contains("injected durable publication rejection"));
    assert!(
        is_application_public_outbox_append_error(&error),
        "typed request-publication context must retain the runtime uncertainty classifier"
    );
    assert_eq!(provider.requests.lock().expect("requests").len(), 1);
    assert!(
        session
            .user_input_projection()?
            .public_requests()
            .is_empty()
    );
    assert!(!session.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::ToolResultV3(result) if result.tool_name == sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME)),
        "a host publication failure cannot masquerade as model InvalidInput");
    Ok(())
}

#[test]
fn consumed_user_input_resolution_is_exact_and_read_only() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let binding = bind_application_session(
        &config_path,
        temp.path(),
        Some(
            &temp
                .path()
                .join("state/sessions/read-only-settlement.jsonl"),
        ),
    )?;
    let requested = seed_application_user_input_request(&config_path, temp.path(), &binding)?;
    let mut session = Session::load_from_store(
        "custom",
        "gpt-test",
        JsonlSessionStore::open_existing(&binding.session_log_path)?,
    )?;
    sigil_kernel::accept_user_input_decision(
        &mut session,
        sigil_kernel::UserInputDecisionCommandV1 {
            identity: requested.request.identity.clone(),
            request_hash: requested.request_hash.clone(),
            command_id: UserInputCommandId::new("read-only-answer")?,
            decision: UserInputDecisionV1::Submitted {
                answers: vec![UserInputAnswerV1 {
                    question_id: "mode".to_owned(),
                    value: UserInputAnswerValueV1::Text {
                        value: "safe".to_owned(),
                    },
                }],
            },
        },
        20,
    )?;
    sigil_kernel::prepare_user_input_continuation(
        &mut session,
        &requested.request.identity,
        &requested.request_hash,
        "owner",
        "read-only-physical",
        30,
    )?;
    session.append_user_input_lifecycle(vec![UserInputLifecycleEntryV1::Resolved(
        sigil_kernel::UserInputResolvedV1 {
            schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
            identity: requested.request.identity.clone(),
            request_hash: requested.request_hash.clone(),
            resolution: UserInputResolutionV1::Consumed,
            resolved_at_unix_ms: 40,
        },
    )])?;
    let mut context = super::super::user_input::ApplicationUserInputContinuationContext {
        identity: requested.request.identity,
        request_hash: requested.request_hash,
        supervisor_instance_id: "owner".to_owned(),
        physical_attempt_id: "read-only-physical".to_owned(),
    };
    let before = std::fs::read(&binding.session_log_path)?;
    assert!(
        super::super::user_input::resolve_application_user_input_continuation(
            &mut session,
            &context,
            UserInputResolutionV1::Consumed
        )?
        .is_none()
    );
    context.physical_attempt_id = "wrong-physical".to_owned();
    assert!(
        super::super::user_input::resolve_application_user_input_continuation(
            &mut session,
            &context,
            UserInputResolutionV1::Consumed
        )
        .is_err()
    );
    context.request_hash = format!("sha256:{}", "b".repeat(64));
    assert!(
        super::super::user_input::resolve_application_user_input_continuation(
            &mut session,
            &context,
            UserInputResolutionV1::Consumed
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&binding.session_log_path)?, before);
    Ok(())
}

#[test]
fn application_run_and_user_input_types_stay_compact_for_small_stacks() {
    assert!(
        std::mem::size_of::<super::super::ApplicationRunExecution>() <= 2048,
        "ApplicationRunExecution grew to {} bytes; box large embedded fields to preserve Windows 1 MiB stack headroom",
        std::mem::size_of::<super::super::ApplicationRunExecution>()
    );
    assert!(
        std::mem::size_of::<super::super::PreparedApplicationRun>() <= 2560,
        "PreparedApplicationRun grew to {} bytes; box large embedded fields to preserve Windows 1 MiB stack headroom",
        std::mem::size_of::<super::super::PreparedApplicationRun>()
    );
    assert!(
        std::mem::size_of::<super::super::PreparedApplicationUserInputDecision>() <= 2560,
        "PreparedApplicationUserInputDecision grew to {} bytes; box large embedded fields to preserve Windows 1 MiB stack headroom",
        std::mem::size_of::<super::super::PreparedApplicationUserInputDecision>()
    );
}
