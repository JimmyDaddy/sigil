use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use sigil_kernel::{
    Agent, ControlEntry, JsonlSessionStore, ProviderChunk, ReasoningEffort, Session,
    SessionLogEntry, ToolCall, ToolRegistry, UserInputAnswerV1, UserInputAnswerValueV1,
    UserInputCommandId, UserInputDecisionCommandV1, UserInputDecisionV1, UserInputSourceV1,
    UserInputStatusV1, accept_user_input_decision,
};

use super::{
    super::{WorkerCommand, WorkerMessage},
    common::{
        PlannedProvider, StreamPlan, routed_unauthenticated_test_root_config, spawn_test_worker,
        spawn_test_worker_with_existing_authority_composition,
    },
};

#[test]
fn ordinary_user_input_recovery_after_restart_requires_exact_receipt_and_resumes_once() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path =
        workspace_root.join(".sigil/sessions/session-ordinary-input-recovery.jsonl");
    let root_config = routed_unauthenticated_test_root_config(&workspace_root, "planned-model");
    let question_args = r#"{
        "questions": [{
            "id": "scope",
            "question": "Which subsystem should the agent inspect?"
        }]
    }"#;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::ToolCallStart {
            id: "ordinary-recovery-question".to_owned(),
            name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
        },
        ProviderChunk::ToolCallArgsDelta {
            id: "ordinary-recovery-question".to_owned(),
            delta: question_args.to_owned(),
        },
        ProviderChunk::ToolCallComplete(ToolCall {
            id: "ordinary-recovery-question".to_owned(),
            name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
            args_json: question_args.to_owned(),
        }),
        ProviderChunk::Done,
    ])]);
    let mut original = spawn_test_worker(
        root_config.clone(),
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root.clone(),
    )?;
    original.send(WorkerCommand::SubmitPrompt {
        prompt: "ask which subsystem to inspect before continuing".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let requested = original.recv_until_with_timeout_diagnostic(
        "ordinary agent question",
        Duration::from_secs(20),
        |message| matches!(message, WorkerMessage::UserInputRequested { .. }),
    )?;
    let WorkerMessage::UserInputRequested { request, .. } = requested else {
        unreachable!("request predicate only returns UserInputRequested");
    };
    assert_eq!(request.source, UserInputSourceV1::Agent);
    let authority_composition = original.authority_composition();
    original.stop()?;

    // Model a crash after the durable answer receipt and before a new execution owner starts.
    // The previous worker is fully stopped before this fixture opens the isolated session.
    let command = UserInputDecisionCommandV1 {
        identity: request.identity.clone(),
        request_hash: request.request_hash.clone(),
        command_id: UserInputCommandId::new("ordinary-recovery-answer")?,
        decision: UserInputDecisionV1::Submitted {
            answers: vec![UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: UserInputAnswerValueV1::Text {
                    value: "runtime recovery".to_owned(),
                },
            }],
        },
    };
    {
        let mut session = Session::load_from_store(
            "planned",
            "planned-model",
            JsonlSessionStore::new(&session_log_path)?,
        )?;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_millis()
            .try_into()?;
        let receipt = accept_user_input_decision(&mut session, command.clone(), now_ms)?;
        assert!(receipt.continuation_required);
        assert!(!receipt.idempotent_replay);
        assert_eq!(
            session
                .user_input_projection()?
                .request(&request.identity)
                .context("the accepted ordinary request must remain durable")?
                .status,
            UserInputStatusV1::DecisionAccepted,
        );
    }

    let (provider, stream_started_rx) = PlannedProvider::new_with_stream_start_signal(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("continued from the durable answer".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Fail("a recovered answer must never execute the provider twice"),
    ]);
    let restarted = spawn_test_worker_with_existing_authority_composition(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
        authority_composition,
    )?;
    let recovered = restarted.recv_until_with_timeout_diagnostic(
        "ordinary accepted answer recovered after startup",
        Duration::from_secs(20),
        |message| matches!(message, WorkerMessage::RecoveredUserInputAttention { .. }),
    )?;
    let WorkerMessage::RecoveredUserInputAttention {
        command: recovered, ..
    } = recovered
    else {
        unreachable!("recovery predicate only returns RecoveredUserInputAttention");
    };
    assert_eq!(
        recovered, command,
        "startup must recover the original durable command"
    );
    assert!(matches!(
        stream_started_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));

    for (generation, expected_request_hash) in [
        (
            request.identity.generation + 1,
            request.request_hash.clone(),
        ),
        (
            request.identity.generation,
            format!("{}-mismatch", request.request_hash),
        ),
    ] {
        restarted.send(WorkerCommand::ResumeRecoveredUserInput {
            command_id: command.command_id.as_str().to_owned(),
            request_id: request.identity.request_id.as_str().to_owned(),
            generation,
            expected_request_hash: expected_request_hash.clone(),
        })?;
        let failed = restarted.recv_until_with_timeout_diagnostic(
            "mismatched ordinary recovery rejected",
            Duration::from_secs(10),
            |message| matches!(message, WorkerMessage::UserInputDecisionFailed { .. }),
        )?;
        let WorkerMessage::UserInputDecisionFailed {
            request_id,
            generation: rejected_generation,
            expected_request_hash: rejected_hash,
            ..
        } = failed
        else {
            unreachable!("failure predicate only returns UserInputDecisionFailed");
        };
        assert_eq!(request_id, request.identity.request_id.as_str());
        assert_eq!(rejected_generation, generation);
        assert_eq!(rejected_hash, expected_request_hash);
        assert!(matches!(
            stream_started_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
    }

    let resume = || WorkerCommand::ResumeRecoveredUserInput {
        command_id: command.command_id.as_str().to_owned(),
        request_id: request.identity.request_id.as_str().to_owned(),
        generation: request.identity.generation,
        expected_request_hash: request.request_hash.clone(),
    };
    restarted.send(resume())?;
    let applied = restarted.recv_until_with_timeout_diagnostic(
        "exact ordinary recovery accepted",
        Duration::from_secs(10),
        |message| matches!(message, WorkerMessage::UserInputDecisionApplied { .. }),
    )?;
    let WorkerMessage::UserInputDecisionApplied {
        request: applied,
        continuation_started,
        ..
    } = applied
    else {
        unreachable!("applied predicate only returns UserInputDecisionApplied");
    };
    assert_eq!(applied.identity, request.identity);
    assert_eq!(applied.request_hash, request.request_hash);
    assert!(continuation_started);
    let finished = restarted.recv_until_with_timeout_diagnostic(
        "ordinary recovered continuation completed",
        Duration::from_secs(20),
        |message| matches!(message, WorkerMessage::RunFinished { .. }),
    )?;
    let WorkerMessage::RunFinished { result, entries } = finished else {
        unreachable!("finish predicate only returns RunFinished");
    };
    assert_eq!(result.final_text, "continued from the durable answer");
    stream_started_rx.recv_timeout(Duration::from_secs(1))?;
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry,
                SessionLogEntry::Control(ControlEntry::UserInputContinuationStarted(started))
                    if started.identity == request.identity
            ))
            .count(),
        1
    );

    restarted.send(resume())?;
    let duplicate = restarted.recv_until_with_timeout_diagnostic(
        "resolved ordinary recovery rejected without another provider call",
        Duration::from_secs(10),
        |message| matches!(message, WorkerMessage::UserInputDecisionFailed { .. }),
    )?;
    assert!(matches!(duplicate, WorkerMessage::UserInputDecisionFailed {
        request_id, generation, expected_request_hash, ..
    } if request_id == request.identity.request_id.as_str()
        && generation == request.identity.generation
        && expected_request_hash == request.request_hash));
    assert!(matches!(
        stream_started_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    let session = Session::load_from_store(
        "planned",
        "planned-model",
        JsonlSessionStore::new(&session_log_path)?,
    )?;
    assert_eq!(
        session
            .user_input_projection()?
            .request(&request.identity)
            .context("completed ordinary request must remain durable")?
            .status,
        UserInputStatusV1::Resolved,
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(entry,
                SessionLogEntry::Control(ControlEntry::UserInputDecisionAccepted(accepted))
                    if accepted.identity == request.identity
            ))
            .count(),
        1
    );
    restarted.shutdown()?;
    Ok(())
}

#[test]
fn tui_sequential_user_input_suspensions_settle_once_with_and_without_restart() -> Result<()> {
    for restart_between_questions in [false, true] {
        let temp = tempfile::tempdir()?;
        let workspace = temp.path().to_path_buf();
        let session_path = workspace.join(".sigil/sessions/two-questions.jsonl");
        let root_config = routed_unauthenticated_test_root_config(&workspace, "planned-model");
        let question = |ordinal| {
            StreamPlan::Chunks(vec![
                ProviderChunk::ToolCallComplete(ToolCall {
                    id: format!("tui-sequential-{ordinal}"),
                    name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
                    args_json: serde_json::json!({"questions":[{
                        "id":"scope", "question":format!("Question {ordinal}?")
                    }]})
                    .to_string(),
                }),
                ProviderChunk::Done,
            ])
        };
        let provider = PlannedProvider::new(vec![
            question(1),
            question(2),
            StreamPlan::Chunks(vec![
                ProviderChunk::TextDelta("Both TUI answers consumed once.".to_owned()),
                ProviderChunk::Done,
            ]),
        ]);
        // Retain the first stopped worker: it owns the authority fixture's temporary root.
        let mut workers = vec![spawn_test_worker(
            root_config.clone(),
            session_path.clone(),
            Agent::new(provider.clone(), ToolRegistry::new()),
            workspace.clone(),
        )?];
        workers[0].send(WorkerCommand::SubmitPrompt {
            prompt: "Clarify two decisions, then finish.".to_owned(),
            reasoning_effort: ReasoningEffort::Max,
        })?;
        let mut references: Vec<sigil_kernel::PublicUserInputRequestV1> = Vec::new();
        for ordinal in 1..=2 {
            let worker = workers.last().expect("current worker");
            let request = loop {
                match worker.recv_with_timeout(Duration::from_secs(20))? {
                    WorkerMessage::UserInputRequested { request, .. }
                        if references
                            .iter()
                            .all(|prior| prior.identity != request.identity) =>
                    {
                        break request;
                    }
                    WorkerMessage::RunFailed(error) => {
                        anyhow::bail!("question {ordinal} failed after settlement: {error}");
                    }
                    WorkerMessage::UserInputDecisionFailed { message, .. } => {
                        anyhow::bail!("question {ordinal} decision failed: {message}");
                    }
                    WorkerMessage::RunFinished { .. } => {
                        anyhow::bail!("question {ordinal} was skipped");
                    }
                    _ => {}
                }
            };
            assert_eq!(request.status, UserInputStatusV1::Requested);
            references.push(request.clone());
            if restart_between_questions {
                let composition = worker.authority_composition();
                workers.last_mut().expect("current worker").stop()?;
                workers.push(spawn_test_worker_with_existing_authority_composition(
                    root_config.clone(),
                    session_path.clone(),
                    Agent::new(provider.clone(), ToolRegistry::new()),
                    workspace.clone(),
                    composition,
                )?);
            }
            workers.last().expect("current worker").send(
                WorkerCommand::SubmitUserInputDecision {
                    command_id: Some(format!("tui-sequential-answer-{ordinal}")),
                    request_id: request.identity.request_id.as_str().to_owned(),
                    generation: request.identity.generation,
                    expected_request_hash: request.request_hash.clone(),
                    decision: UserInputDecisionV1::Submitted {
                        answers: vec![UserInputAnswerV1 {
                            question_id: "scope".to_owned(),
                            value: UserInputAnswerValueV1::Text {
                                value: format!("answer {ordinal}"),
                            },
                        }],
                    },
                },
            )?;
        }
        loop {
            match workers
                .last()
                .expect("current worker")
                .recv_with_timeout(Duration::from_secs(20))?
            {
                WorkerMessage::RunFinished { result, .. } => {
                    assert_eq!(result.final_text, "Both TUI answers consumed once.");
                    break;
                }
                WorkerMessage::RunFailed(error) => {
                    anyhow::bail!("final continuation failed after settlement: {error}");
                }
                WorkerMessage::UserInputDecisionFailed { message, .. } => {
                    anyhow::bail!("final decision failed: {message}");
                }
                _ => {}
            }
        }
        workers.last_mut().expect("current worker").stop()?;
        assert_eq!(provider.remaining_plan_count(), 0);
        let session = Session::load_from_store(
            "planned",
            "planned-model",
            JsonlSessionStore::open_existing(&session_path)?,
        )?;
        assert_eq!(session.user_input_projection()?.pending().count(), 0);
        assert!(sigil_kernel::recoverable_user_input_decision(&session)?.is_none());
        assert_eq!(
            references[0].identity.root_logical_run_id,
            references[1].identity.root_logical_run_id
        );
        let records = JsonlSessionStore::read_event_records(&session_path)?;
        let public = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
        assert!(!public.events_in_order().iter().any(|entry| matches!(
            entry.event.event,
            sigil_kernel::PublicRunEventKind::RunFailed { .. }
        )));
        for (index, request) in references.iter().enumerate() {
            assert_eq!(
                session
                    .entries()
                    .iter()
                    .filter(|entry| matches!(entry,
                SessionLogEntry::ToolResultV3(result)
                    if result.call_id == format!("tui-sequential-{}", index + 1)))
                    .count(),
                1
            );
            assert_eq!(
                session
                    .entries()
                    .iter()
                    .filter(|entry| matches!(entry,
                SessionLogEntry::Control(ControlEntry::UserInputResolved(resolved))
                    if resolved.identity == request.identity))
                    .count(),
                1
            );
            assert_eq!(public.events_in_order().iter().filter(|entry| matches!(&entry.event.event,
                sigil_kernel::PublicRunEventKind::UserInputChanged { request_id, status: UserInputStatusV1::Resolved, .. }
                    if request_id == request.identity.request_id.as_str())).count(), 1,
                "the TUI outer settlement must not duplicate the kernel's atomic Consumed event");
        }
    }
    Ok(())
}
