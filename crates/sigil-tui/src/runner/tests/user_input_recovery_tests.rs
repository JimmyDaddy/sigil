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
