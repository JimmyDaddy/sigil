use std::{collections::BTreeSet, fs, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use sigil_kernel::{
    Agent, ControlEntry, JsonlSessionStore, PlanArtifactProjection, PlanTaskStartMode,
    ProviderChunk, ReasoningEffort, Session, SessionLogEntry, TaskRoutingPolicy, TaskRunStatus,
    ToolCall, ToolRegistry,
};
use tempfile::tempdir;

use super::{
    super::{WorkerCommand, WorkerMessage},
    common::{
        PlannedProvider, StreamPlan, TestWorker, routed_session_identity, routed_test_root_config,
        routed_unauthenticated_test_root_config, spawn_test_worker,
        spawn_test_worker_with_existing_authority_composition, submit_plan_review_result_chunks,
    },
};

fn assert_finished_public_root(
    session_log_path: &std::path::Path,
    workspace_root: &std::path::Path,
    root_config: &sigil_kernel::RootConfig,
) -> Result<()> {
    use sigil_runtime::RuntimeApplicationProjectionSource;

    let records = JsonlSessionStore::read_event_records(session_log_path)?;
    let lifecycle = records
        .iter()
        .map(sigil_kernel::conversation_run_lifecycle_record_from_stream)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let [
        sigil_kernel::ConversationRunLifecycleRecordV1::ConversationRunStartedV1(started),
        sigil_kernel::ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(finalized),
    ] = lifecycle.as_slice()
    else {
        panic!("the completed handoff must close its one public foreground root");
    };
    assert_eq!(started.run_id(), finalized.run_id());
    assert_eq!(
        finalized.status(),
        sigil_kernel::ConversationRunTerminalStatusV1::Succeeded
    );
    let final_message_id = finalized
        .final_message_id()
        .expect("a successful root must bind a durable assistant message");
    assert!(
        JsonlSessionStore::read_entries(session_log_path)?
            .iter()
            .any(|entry| matches!(entry, SessionLogEntry::Assistant(message) if message.id == final_message_id))
    );
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    let root_events = outbox
        .events_in_order()
        .into_iter()
        .filter(|entry| entry.run_id == started.run_id())
        .collect::<Vec<_>>();
    assert_eq!(
        root_events
            .iter()
            .filter(|entry| matches!(
                entry.event.event,
                sigil_kernel::PublicRunEventKind::RunStarted { .. }
            ))
            .count(),
        1
    );
    assert_eq!(
        root_events
            .iter()
            .filter(|entry| matches!(
                entry.event.event,
                sigil_kernel::PublicRunEventKind::RunFinished { .. }
            ))
            .count(),
        1
    );
    assert!(matches!(
        root_events.last().map(|entry| &entry.event.event),
        Some(sigil_kernel::PublicRunEventKind::RunFinished { .. })
    ));

    let config_path = workspace_root.join("sigil.toml");
    fs::write(&config_path, toml::to_string(root_config)?)?;
    let binding = sigil_runtime::RuntimeSessionProjectionBinding::new(
        config_path,
        workspace_root.to_path_buf(),
        session_log_path.to_path_buf(),
        records[0].session_id().to_owned(),
        sigil_application::ApplicationInstanceId::new("handoff-public-root-test")?,
        sigil_application::AuthenticatedSubject::new("local-user")?,
        Some(sigil_application::WorkspaceScopeId::new(
            "fixture-workspace",
        )?),
        1,
        1,
        1,
        1,
    )?
    .with_owner(sigil_runtime::RuntimeSessionProjectionOwner::from_store(
        &JsonlSessionStore::new(session_log_path)?,
    ));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let snapshot = runtime.block_on(binding.open_projection(
        sigil_application::OpenProjectionRequest {
            scope: binding.scope().clone(),
            observer_generation: 1,
            resume_from: None,
        },
    ))?;
    assert_eq!(snapshot.envelope.projection.run.status.as_str(), "finished");
    assert!(
        snapshot.envelope.projection.run.active_binding.is_none(),
        "the real application projection must release the completed handoff's root run"
    );
    Ok(())
}

/// Initializes a fresh test session through the real route-bound `Session` API before any worker
/// opens it. The worker bootstrap intentionally preserves an existing identity rather than
/// deriving a connection route from configuration, so switch fixtures must model a session whose
/// route was selected at creation time.
fn initialize_routed_plan_review_session(
    root_config: &sigil_kernel::RootConfig,
    session_log_path: &std::path::Path,
    model_name: &str,
) -> Result<()> {
    let identity = routed_session_identity(root_config, model_name)?;
    let sigil_kernel::ControlEntry::SessionIdentity {
        provider_name,
        resolved_model_route: Some(route),
        ..
    } = &identity
    else {
        return Err(anyhow!(
            "routed test identity must contain a resolved model route"
        ));
    };
    let store = JsonlSessionStore::new(session_log_path)?;
    let mut session =
        Session::new_with_route(provider_name.clone(), route.clone()).with_store(store);
    session.append_control(identity)?;
    Ok(())
}

/// Starts an actual composed TUI worker through the explicit PlanReview flow and leaves its
/// managed research child suspended on a real input request.  The private-recovery cases below
/// add a child receipt after this point to model a process that accepted the child decision but
/// exited before its parent Waiting attempt was settled.
fn waiting_managed_plan_review_research_worker(
    session_name: &str,
) -> Result<(
    tempfile::TempDir,
    std::path::PathBuf,
    TestWorker,
    sigil_kernel::PublicUserInputRequestV1,
)> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions")
        .join(format!("{session_name}.jsonl"));
    let question_args = r#"{
        "questions": [{
            "id": "scope",
            "question": "Which module should be migrated first?"
        }]
    }"#;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::ToolCallStart {
            id: "ask-managed-research".to_owned(),
            name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
        },
        ProviderChunk::ToolCallArgsDelta {
            id: "ask-managed-research".to_owned(),
            delta: question_args.to_owned(),
        },
        ProviderChunk::ToolCallComplete(ToolCall {
            id: "ask-managed-research".to_owned(),
            name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
            args_json: question_args.to_owned(),
        }),
        ProviderChunk::Done,
    ])]);
    let root_config = routed_unauthenticated_test_root_config(&workspace_root, "planned-model");
    initialize_routed_plan_review_session(&root_config, &session_log_path, "planned-model")?;
    let worker = spawn_test_worker(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
    )?;
    worker.send(WorkerCommand::SubmitPlanPrompt {
        prompt: "prepare a migration plan".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker
        .recv_until(|message| matches!(message, WorkerMessage::PlanRunStarted { .. }))
        .context("explicit plan review did not start")?;
    let requested = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::UserInputRequested { .. })
    })?;
    let WorkerMessage::UserInputRequested { request, .. } = requested else {
        unreachable!("recv_until only returns UserInputRequested");
    };
    if !matches!(
        &request.source,
        sigil_kernel::UserInputSourceV1::PlanReviewResearch { .. }
    ) {
        return Err(anyhow!(
            "explicit plan review must suspend on a managed PlanReviewResearch request"
        ));
    }
    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    let projection = sigil_kernel::PlanReviewProjection::from_entries(&entries);
    let attempt = projection
        .reviews()
        .next()
        .and_then(sigil_kernel::PlanReviewProjectionEntry::latest_attempt)
        .context("explicit managed plan review lost its Waiting attempt")?;
    assert_eq!(
        attempt.explicit_objective.as_deref(),
        Some("prepare a migration plan"),
        "the source-less /plan lifecycle must carry its safe durable recovery objective"
    );
    Ok((temp, session_log_path, worker, request))
}

/// Writes an authentic child receipt into the exact composed `research/0` log.
/// This is setup for the worker consumer test, not a synthetic event: the child request was
/// emitted by the preceding PlanReview run and the same authority writer admits and settles the
/// session-log namespace around the durable receipt append.
fn persist_managed_plan_review_research_decision(
    worker: &TestWorker,
    request: &sigil_kernel::PublicUserInputRequestV1,
    command_id: &str,
    decision: sigil_kernel::UserInputDecisionV1,
) -> Result<sigil_kernel::UserInputDecisionCommandV1> {
    use sigil_runtime::managed_storage_writer::StorageWriterChannelV1;

    let sigil_kernel::UserInputSourceV1::PlanReviewResearch { attempt_id, .. } = &request.source
    else {
        return Err(anyhow!(
            "only a PlanReviewResearch request owns a managed research child"
        ));
    };
    let key = format!("pr-{}-research-0", attempt_id.as_str());
    let writer = worker.managed_storage_writer();
    let lease = writer
        .acquire_named(StorageWriterChannelV1::SessionLog, &key)
        .context("failed to admit the existing managed plan-review research SessionLog")?;
    let result = (|| -> Result<_> {
        let store = JsonlSessionStore::new(lease.path().join("records.jsonl"))?;
        let mut child = Session::load_from_store("planned", "planned-model", store)?;
        if child.session_scope_id() != request.identity.session_scope_id.as_str() {
            return Err(anyhow!(
                "managed child receipt belongs to another child session scope"
            ));
        }
        let command = sigil_kernel::UserInputDecisionCommandV1 {
            identity: request.identity.clone(),
            request_hash: request.request_hash.clone(),
            command_id: sigil_kernel::UserInputCommandId::new(command_id.to_owned())?,
            decision,
        };
        let receipt = sigil_kernel::accept_user_input_decision(&mut child, command.clone(), 120)?;
        assert!(
            !receipt.idempotent_replay,
            "fixture must append a new child receipt"
        );
        assert_eq!(receipt.request.identity, command.identity);
        assert_eq!(receipt.request.request_hash, command.request_hash);
        let child_projection = child.user_input_projection()?;
        let state = child_projection
            .request(&command.identity)
            .context("managed child receipt was not projected")?;
        let accepted = state
            .decision
            .as_ref()
            .context("managed child receipt has no durable decision")?;
        assert_eq!(accepted.identity, command.identity);
        assert_eq!(accepted.request_hash, command.request_hash);
        assert_eq!(accepted.command_id, command.command_id);
        assert_eq!(
            receipt
                .request
                .answer_receipt
                .as_ref()
                .map(|answer| &answer.command_id),
            Some(&command.command_id),
            "public child receipt must bind the exact accepted command id"
        );
        match (&command.decision, &accepted.decision) {
            (
                sigil_kernel::UserInputDecisionV1::Submitted {
                    answers: expected_answers,
                },
                sigil_kernel::UserInputDurableDecisionV1::Submitted {
                    answer_hash,
                    answered_question_ids,
                    answers: Some(actual_answers),
                },
            ) => {
                assert_eq!(actual_answers, expected_answers);
                assert_eq!(
                    answered_question_ids,
                    &expected_answers
                        .iter()
                        .map(|answer| answer.question_id.clone())
                        .collect::<Vec<_>>()
                );
                assert!(!answer_hash.is_empty());
                assert_eq!(
                    state.status,
                    sigil_kernel::UserInputStatusV1::DecisionAccepted,
                    "a submitted child answer remains pending until the parent continuation"
                );
                assert!(receipt.continuation_required);
                assert!(receipt.request.resolution.is_none());
                assert!(matches!(
                    receipt.request.answer_receipt,
                    Some(sigil_kernel::PublicUserInputAnswerReceiptV1 {
                        decision: sigil_kernel::PublicUserInputDecisionKindV1::Submitted,
                        answer_hash: Some(_),
                        ..
                    })
                ));
            }
            (
                sigil_kernel::UserInputDecisionV1::RunCancelled,
                sigil_kernel::UserInputDurableDecisionV1::RunCancelled,
            ) => {
                assert_eq!(state.status, sigil_kernel::UserInputStatusV1::Resolved);
                assert!(!receipt.continuation_required);
                assert_eq!(
                    receipt.request.resolution,
                    Some(sigil_kernel::UserInputResolutionV1::RunCancelled)
                );
                assert!(matches!(
                    receipt.request.answer_receipt,
                    Some(sigil_kernel::PublicUserInputAnswerReceiptV1 {
                        decision: sigil_kernel::PublicUserInputDecisionKindV1::RunCancelled,
                        answer_hash: None,
                        ..
                    })
                ));
            }
            (
                sigil_kernel::UserInputDecisionV1::Declined,
                sigil_kernel::UserInputDurableDecisionV1::Declined,
            ) => {
                assert_eq!(state.status, sigil_kernel::UserInputStatusV1::Resolved);
                assert!(!receipt.continuation_required);
                assert_eq!(
                    receipt.request.resolution,
                    Some(sigil_kernel::UserInputResolutionV1::Declined)
                );
                assert!(matches!(
                    receipt.request.answer_receipt,
                    Some(sigil_kernel::PublicUserInputAnswerReceiptV1 {
                        decision: sigil_kernel::PublicUserInputDecisionKindV1::Declined,
                        answer_hash: None,
                        ..
                    })
                ));
            }
            _ => {
                return Err(anyhow!(
                    "managed child receipt changed the requested decision"
                ));
            }
        }
        Ok(command)
    })();
    writer
        .finalize(lease)
        .map(|_| ())
        .context("failed to settle the managed plan-review research SessionLog receipt")?;
    result
}

fn persist_managed_plan_review_research_cancel(
    worker: &TestWorker,
    request: &sigil_kernel::PublicUserInputRequestV1,
    command_id: &str,
) -> Result<sigil_kernel::UserInputDecisionCommandV1> {
    persist_managed_plan_review_research_decision(
        worker,
        request,
        command_id,
        sigil_kernel::UserInputDecisionV1::RunCancelled,
    )
}

fn persisted_managed_plan_review_research_answer(
    worker: &TestWorker,
    request: &sigil_kernel::PublicUserInputRequestV1,
    command_id: &str,
    answer: &str,
) -> Result<sigil_kernel::UserInputDecisionCommandV1> {
    persist_managed_plan_review_research_decision(
        worker,
        request,
        command_id,
        sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: answer.to_owned(),
                },
            }],
        },
    )
}

fn assert_waiting_plan_review_recovery_entries(
    entries: &[SessionLogEntry],
    private_answer: &str,
) -> Result<()> {
    let timeline = serde_json::to_string(entries)?;
    assert!(
        !timeline.contains(private_answer),
        "a managed child answer must not leak into parent session entries or the public timeline projection"
    );
    let projection = sigil_kernel::PlanReviewProjection::from_entries(entries);
    let latest_attempt = projection
        .reviews()
        .next()
        .and_then(sigil_kernel::PlanReviewProjectionEntry::latest_attempt)
        .context("recovery lost the parent plan-review attempt")?;
    assert_eq!(
        latest_attempt.status,
        sigil_kernel::PlanReviewAttemptStatus::WaitingForInput,
        "recovery must not automatically settle the parent Waiting attention fact"
    );
    Ok(())
}

fn assert_exact_recovered_plan_review_command(
    recovered: &sigil_kernel::UserInputDecisionCommandV1,
    expected: &sigil_kernel::UserInputDecisionCommandV1,
) {
    assert_eq!(recovered.identity, expected.identity);
    assert_eq!(recovered.request_hash, expected.request_hash);
    assert_eq!(recovered.command_id, expected.command_id);
    assert_eq!(
        serde_json::to_value(&recovered.decision).expect("recovered decision serializes"),
        serde_json::to_value(&expected.decision).expect("expected decision serializes"),
        "recovery must re-read the durable child decision instead of synthesizing an answer"
    );
}

#[test]
fn plan_review_research_private_resume_replays_the_exact_managed_child_cancel() -> Result<()> {
    let (_temp, session_log_path, worker, request) =
        waiting_managed_plan_review_research_worker("session-private-research-resume")?;
    let command = persist_managed_plan_review_research_cancel(
        &worker,
        &request,
        "private-managed-research-cancel",
    )?;

    // This is the private worker ingress used after the application service has cached the
    // original command as Uncertain.  It carries only public matching fields; the worker must
    // re-read the authoritative child receipt above before it can re-enter the normal dispatcher.
    worker.send(WorkerCommand::ResumeRecoveredUserInput {
        command_id: command.command_id.as_str().to_owned(),
        request_id: request.identity.request_id.as_str().to_owned(),
        generation: request.identity.generation,
        expected_request_hash: request.request_hash.clone(),
    })?;
    let applied = worker.recv_until_with_timeout_diagnostic(
        "private plan-review resume dispatch",
        Duration::from_secs(10),
        |message| matches!(message, WorkerMessage::UserInputDecisionApplied { .. }),
    )?;
    let WorkerMessage::UserInputDecisionApplied {
        request: applied_request,
        continuation_started,
        entries,
    } = applied
    else {
        unreachable!("recv_until only returns UserInputDecisionApplied");
    };
    assert_eq!(applied_request.identity, request.identity);
    assert_eq!(applied_request.request_hash, request.request_hash);
    assert!(
        !continuation_started,
        "a recovered child cancel must not spawn a new plan review"
    );
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
            if attempt.status == sigil_kernel::PlanReviewAttemptStatus::Cancelled
    )));
    let durable_entries = JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(
        durable_entries.iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
                if attempt.status == sigil_kernel::PlanReviewAttemptStatus::Cancelled
        )),
        "the worker dispatcher must settle the real parent Waiting attempt"
    );
    worker.shutdown()?;
    Ok(())
}

#[test]
fn plan_review_research_private_resume_rejects_a_mismatched_managed_child_receipt() -> Result<()> {
    let (_temp, session_log_path, worker, request) =
        waiting_managed_plan_review_research_worker("session-private-research-mismatch")?;
    let command = persist_managed_plan_review_research_cancel(
        &worker,
        &request,
        "private-managed-research-mismatch",
    )?;

    worker.send(WorkerCommand::ResumeRecoveredUserInput {
        command_id: command.command_id.as_str().to_owned(),
        request_id: request.identity.request_id.as_str().to_owned(),
        generation: request.identity.generation,
        expected_request_hash: format!("{}-mismatch", request.request_hash),
    })?;
    let notice = worker.recv_until_with_timeout_diagnostic(
        "private plan-review mismatch rejection",
        Duration::from_secs(10),
        |message| matches!(message, WorkerMessage::UserInputDecisionFailed { message, .. } if message.contains("no longer matches")),
    )?;
    assert!(matches!(
        notice,
        WorkerMessage::UserInputDecisionFailed { ref request_id, generation, ref expected_request_hash, ref message, .. }
            if request_id == request.identity.request_id.as_str()
                && generation == request.identity.generation
                && expected_request_hash == &format!("{}-mismatch", request.request_hash)
                && message == "recovered input no longer matches the selected request"
    ));
    let durable_entries = JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(
        durable_entries.iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
                if attempt.status == sigil_kernel::PlanReviewAttemptStatus::WaitingForInput
        )),
        "a mismatched private resume must not settle the parent attention fact"
    );
    assert!(
        !durable_entries.iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
                if attempt.status == sigil_kernel::PlanReviewAttemptStatus::Cancelled
        )),
        "a mismatch must not dispatch the recovered child decision"
    );
    worker.shutdown()?;
    Ok(())
}

#[test]
fn startup_recovers_managed_plan_review_research_attention_without_executing_private_answer()
-> Result<()> {
    let (temp, session_log_path, mut original, request) =
        waiting_managed_plan_review_research_worker("session-restart-private-research")?;
    let private_answer = "restart-only managed answer";
    let accepted = persisted_managed_plan_review_research_answer(
        &original,
        &request,
        "restart-private-managed-answer",
        private_answer,
    )?;
    let authority_composition = original.authority_composition();
    original.stop()?;

    let workspace_root = temp.path().to_path_buf();
    let restarted = spawn_test_worker_with_existing_authority_composition(
        routed_unauthenticated_test_root_config(&workspace_root, "planned-model"),
        session_log_path.clone(),
        Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
        workspace_root,
        authority_composition,
    )?;
    let recovered = restarted.recv_until_with_timeout_diagnostic(
        "startup managed plan-review recovery",
        Duration::from_secs(10),
        |message| matches!(message, WorkerMessage::RecoveredUserInputAttention { .. }),
    )?;
    let WorkerMessage::RecoveredUserInputAttention { command, entries } = recovered else {
        unreachable!("recovery predicate only returns its private worker message");
    };
    assert_exact_recovered_plan_review_command(&command, &accepted);
    assert_waiting_plan_review_recovery_entries(&entries, private_answer)?;

    let durable_entries = JsonlSessionStore::read_entries(&session_log_path)?;
    assert_waiting_plan_review_recovery_entries(&durable_entries, private_answer)?;
    restarted.shutdown()?;
    Ok(())
}

#[test]
fn switch_recovers_managed_plan_review_research_attention_after_session_switched() -> Result<()> {
    let (temp, target_session_log_path, mut original, request) =
        waiting_managed_plan_review_research_worker("session-switch-private-research")?;
    let private_answer = "switch-only managed answer";
    let accepted = persisted_managed_plan_review_research_answer(
        &original,
        &request,
        "switch-private-managed-answer",
        private_answer,
    )?;
    let authority_composition = original.authority_composition();
    original.stop()?;

    let workspace_root = temp.path().to_path_buf();
    let current_session_log_path = temp
        .path()
        .join(".sigil/sessions/session-before-managed-recovery-switch.jsonl");
    let worker = spawn_test_worker_with_existing_authority_composition(
        routed_unauthenticated_test_root_config(&workspace_root, "planned-model"),
        current_session_log_path,
        Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
        workspace_root,
        authority_composition,
    )?;
    let ready = worker.recv_until(|message| matches!(message, WorkerMessage::WorkerReady))?;
    assert!(matches!(ready, WorkerMessage::WorkerReady));

    worker.send(WorkerCommand::SwitchSession {
        session_log_path: target_session_log_path.clone(),
        attachment_recovery_binding: None,
    })?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut switched = false;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!(
                "timed out waiting for private plan-review recovery after SessionSwitched"
            ));
        }
        let message = worker.recv_with_timeout(remaining)?;
        match message {
            WorkerMessage::SessionSwitched {
                session_log_path,
                entries,
                ..
            } => {
                assert_eq!(session_log_path, target_session_log_path);
                assert_waiting_plan_review_recovery_entries(&entries, private_answer)?;
                switched = true;
            }
            WorkerMessage::RecoveredUserInputAttention { command, entries } => {
                assert!(
                    switched,
                    "a session switch must reach the App before its private managed-input recovery"
                );
                assert_exact_recovered_plan_review_command(&command, &accepted);
                assert_waiting_plan_review_recovery_entries(&entries, private_answer)?;
                break;
            }
            WorkerMessage::SessionAttachmentTransferred { .. } => {}
            unexpected => {
                return Err(anyhow!(
                    "unexpected worker message while switching to managed plan-review recovery: {unexpected:?}"
                ));
            }
        }
    }

    let durable_entries = JsonlSessionStore::read_entries(&target_session_log_path)?;
    assert_waiting_plan_review_recovery_entries(&durable_entries, private_answer)?;
    worker.shutdown()?;
    Ok(())
}

#[test]
fn explicit_plan_persists_public_start_before_immediate_cancel() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/explicit-plan-early-cancel.jsonl");
    let root_config = routed_unauthenticated_test_root_config(&workspace_root, "planned-model");
    initialize_routed_plan_review_session(&root_config, &session_log_path, "planned-model")?;
    let provider = PlannedProvider::new(vec![StreamPlan::GatedChunks {
        gate: Arc::new(tokio::sync::Notify::new()),
        chunks: vec![
            ProviderChunk::TextDelta("must not finish".to_owned()),
            ProviderChunk::Done,
        ],
    }]);
    let worker = spawn_test_worker(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
    )?;
    worker.send(WorkerCommand::SubmitPlanPrompt {
        prompt: "cancel this plan immediately after admission".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let mut observed_started = false;
    loop {
        match worker.recv_with_timeout(Duration::from_secs(10))? {
            WorkerMessage::PlanRunStarted { .. } => {
                // Cancel at admission without waiting for the Plan coordinator or provider.
                // Before this message the urgent command could overtake SubmitPlanPrompt.
                worker.send(WorkerCommand::CancelRun)?;
                let records = JsonlSessionStore::read_event_records(&session_log_path)?;
                assert!(records.iter().any(|record| matches!(
                    sigil_kernel::conversation_run_lifecycle_record_from_stream(record),
                    Ok(Some(sigil_kernel::ConversationRunLifecycleRecordV1::ConversationRunStartedV1(_)))
                )), "native PlanRunStarted must follow durable foreground admission");
                observed_started = true;
            }
            WorkerMessage::RunCancelled { .. } => break,
            WorkerMessage::RunFailed(error) => {
                return Err(anyhow!("early Plan cancellation failed: {error}"));
            }
            WorkerMessage::RunInterrupted { .. } => {
                return Err(anyhow!("early Plan cancellation was not quiescent"));
            }
            WorkerMessage::PlanRunFinished { .. } => {
                return Err(anyhow!("the gated Plan finished before cancellation"));
            }
            _ => {}
        }
    }
    assert!(observed_started);
    worker.shutdown()?;
    let records = JsonlSessionStore::read_event_records(&session_log_path)?;
    let lifecycle = records
        .iter()
        .map(sigil_kernel::conversation_run_lifecycle_record_from_stream)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let [
        sigil_kernel::ConversationRunLifecycleRecordV1::ConversationRunStartedV1(started),
        sigil_kernel::ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(finalized),
    ] = lifecycle.as_slice()
    else {
        return Err(anyhow!(
            "early Plan cancellation must close exactly one admitted foreground run"
        ));
    };
    assert_eq!(started.run_id(), finalized.run_id());
    assert_eq!(
        finalized.status(),
        sigil_kernel::ConversationRunTerminalStatusV1::Cancelled
    );
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    let root_events = outbox
        .events_in_order()
        .into_iter()
        .filter(|entry| entry.run_id == started.run_id())
        .collect::<Vec<_>>();
    assert!(matches!(
        root_events.first().map(|entry| &entry.event.event),
        Some(sigil_kernel::PublicRunEventKind::RunStarted { .. })
    ));
    assert!(matches!(
        root_events.last().map(|entry| &entry.event.event),
        Some(sigil_kernel::PublicRunEventKind::RunCancelled)
    ));
    assert_eq!(
        root_events
            .iter()
            .filter(|entry| matches!(
                entry.event.event,
                sigil_kernel::PublicRunEventKind::RunStarted { .. }
            ))
            .count(),
        1
    );
    assert_eq!(
        root_events
            .iter()
            .filter(|entry| matches!(
                entry.event.event,
                sigil_kernel::PublicRunEventKind::RunCancelled
            ))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn ordinary_chat_plan_review_route_commits_typed_draft_and_surfaces_plan_ready() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-auto-plan-review-e2e.jsonl");
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    initialize_routed_plan_review_session(&root_config, &session_log_path, "planned-model")?;
    let review_args = r#"{"reason_codes":["architectural_tradeoff","scope_uncertain"]}"#;
    let draft_args = r##"{
  "schema_version": 1,
  "outcome": "draft",
  "content": "# Migrate the coordinator\n\n1. Migrate coordinator\n\nPaths: src/coordinator.rs\n\nChecks: cargo test"
}"##;
    let provider = PlannedProvider::new(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "review-call".to_owned(),
                name: sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "review-call".to_owned(),
                delta: review_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "review-call".to_owned(),
                name: sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
                args_json: review_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "draft-call".to_owned(),
                name: sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "draft-call".to_owned(),
                delta: draft_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "draft-call".to_owned(),
                name: sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned(),
                args_json: draft_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker(
        root_config.clone(),
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root.clone(),
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "design the coordinator migration before touching anything".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::PlanRunFinished { .. })
    })?;
    let WorkerMessage::PlanRunFinished { entries, .. } = finished else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationRouteDecisionRecorded(
                    decision
                )) if decision.route == sigil_kernel::ConversationRoute::PlanReview
            ))
            .count(),
        1,
        "the ordinary tool loop records exactly one accepted PlanReview decision"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::PlanDraftCreated(_))
            ))
            .count(),
        1,
        "plan review commits exactly one typed draft"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
                    if attempt.status == sigil_kernel::PlanReviewAttemptStatus::DraftReady
            ))
            .count(),
        1,
        "plan review attempt reaches DraftReady"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::User(_)))
            .count(),
        1,
        "the original user turn is written exactly once"
    );
    assert!(
        entries.iter().all(|entry| !matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(_))
        )),
        "plan review must not create a task handoff"
    );
    assert_finished_public_root(&session_log_path, &workspace_root, &root_config)?;
    worker.shutdown()?;
    Ok(())
}

#[test]
fn real_plan_review_managed_file_artifact_e2e() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    fs::write(
        workspace_root.join("README.md"),
        "managed plan review evidence\nneedle: authority-owned\n",
    )?;
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-managed-file-plan-review-e2e.jsonl");
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let review_args = r#"{"reason_codes":["architectural_tradeoff","scope_uncertain"]}"#;
    let draft_args = r##"{
  "schema_version": 1,
  "outcome": "draft",
  "content": "# Review the managed workspace evidence\n\n1. Review managed evidence\n\nPaths: README.md\n\nChecks: inspect durable artifact refs"
}"##;
    let provider = PlannedProvider::new(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "review-call".to_owned(),
                name: sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "review-call".to_owned(),
                delta: review_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "review-call".to_owned(),
                name: sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
                args_json: review_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "list-call".to_owned(),
                name: "list_files".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "list-call".to_owned(),
                delta: r#"{"path":".","limit":20}"#.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "list-call".to_owned(),
                name: "list_files".to_owned(),
                args_json: r#"{"path":".","limit":20}"#.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "grep-call".to_owned(),
                name: "grep".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "grep-call".to_owned(),
                delta: r#"{"pattern":"authority-owned","path":"README.md"}"#.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "grep-call".to_owned(),
                name: "grep".to_owned(),
                args_json: r#"{"pattern":"authority-owned","path":"README.md"}"#.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "read-call".to_owned(),
                name: "read_file".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "read-call".to_owned(),
                delta: r#"{"path":"README.md"}"#.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "read-call".to_owned(),
                name: "read_file".to_owned(),
                args_json: r#"{"path":"README.md"}"#.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(submit_plan_review_result_chunks("draft-call", draft_args)),
    ]);
    let mut registry = ToolRegistry::new();
    sigil_tools_builtin::register_builtin_tools(&mut registry);
    let worker = spawn_test_worker(
        root_config,
        session_log_path,
        Agent::new(provider, registry),
        workspace_root,
    )?;
    let authority_root = worker.authority_root_path().to_path_buf();

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "inspect the workspace evidence before proposing the plan".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(20), |message| {
        matches!(message, WorkerMessage::PlanRunFinished { .. })
    })?;
    let WorkerMessage::PlanRunFinished { entries, .. } = finished else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
            if attempt.status == sigil_kernel::PlanReviewAttemptStatus::DraftReady
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanDraftCreated(_))
    )));

    // Exercise the real user acceptance boundary after the managed file evidence run.  A
    // paused task keeps this black-box fixture focused on admission rather than requiring a
    // second provider stream for task execution.
    let draft = PlanArtifactProjection::from_entries(&entries)
        .latest_pending_plan()
        .context("managed file plan review should produce a pending draft")?
        .clone();
    worker.send(WorkerCommand::CreateTaskFromPlan {
        plan_id: draft.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash.clone(),
        start_mode: PlanTaskStartMode::CreatePaused,
        permission_grant: None,
    })?;
    let admitted = worker
        .recv_until_with_timeout(Duration::from_secs(20), |message| {
            matches!(message, WorkerMessage::TaskCreatedFromPlan { .. })
        })
        .context("accepted managed file plan should create a task admission")?;
    let WorkerMessage::TaskCreatedFromPlan {
        entry: task_entry,
        start_mode,
        entries: admitted_entries,
    } = admitted
    else {
        unreachable!("recv_until only returns TaskCreatedFromPlan");
    };
    assert_eq!(start_mode, PlanTaskStartMode::CreatePaused);
    assert_eq!(task_entry.plan_id, draft.plan_id);
    assert_eq!(task_entry.plan_hash, draft.plan_hash);
    let task_projection = sigil_kernel::TaskStateProjection::from_entries(&admitted_entries);
    let task = task_projection
        .tasks
        .get(&task_entry.task_id)
        .context("accepted managed file plan should project a task")?;
    assert!(task.direct_execution_admission.is_some());
    assert_eq!(task.status, TaskRunStatus::Paused);
    assert_eq!(
        PlanArtifactProjection::from_entries(&admitted_entries)
            .latest_decision(&draft.plan_id)
            .context("accepted managed file plan should record its decision")?
            .decision,
        sigil_kernel::PlanDecision::Accepted
    );
    assert!(admitted_entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAdmittedV1(admission))
            if admission.task_id == task_entry.task_id
    )));

    let mut observed_tools = BTreeSet::new();
    let mut artifact_backed_results = 0usize;
    let mut records_files = vec![authority_root.join("state")];
    while let Some(path) = records_files.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                records_files.push(entry?.path());
            }
        } else if path.file_name().and_then(|name| name.to_str()) == Some("records.jsonl") {
            for entry in sigil_kernel::JsonlSessionStore::read_entries(&path)? {
                if let SessionLogEntry::ToolResultV3(recorded) = entry {
                    observed_tools.insert(recorded.tool_name.to_owned());
                    if recorded.artifact.descriptor().is_some() {
                        artifact_backed_results += 1;
                    }
                }
            }
        }
    }
    assert!(observed_tools.contains("list_files"));
    assert!(observed_tools.contains("grep"));
    assert!(observed_tools.contains("read_file"));
    assert_eq!(artifact_backed_results, observed_tools.len());
    worker.shutdown()?;
    Ok(())
}

#[test]
fn plan_revision_runs_supervised_review_returns_session_and_surfaces_new_draft() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-revise-plan-e2e.jsonl");
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let review_args = r#"{"reason_codes":["architectural_tradeoff"]}"#;
    let draft_1_args = r##"{
  "schema_version": 1,
  "outcome": "draft",
  "content": "# Migrate the coordinator\n\n1. Migrate coordinator\n\nPaths: src/coordinator.rs"
}"##;
    let draft_2_args = r##"{
  "schema_version": 1,
  "outcome": "draft",
  "content": "# Revised coordinator migration\n\n1. Revise migration\n\nPaths: src/coordinator.rs"
}"##;
    let provider = PlannedProvider::new(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "review-call".to_owned(),
                name: sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "review-call".to_owned(),
                delta: review_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "review-call".to_owned(),
                name: sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
                args_json: review_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "draft-call".to_owned(),
                name: sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "draft-call".to_owned(),
                delta: draft_1_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "draft-call".to_owned(),
                name: sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned(),
                args_json: draft_1_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "revision-draft-call".to_owned(),
                name: sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "revision-draft-call".to_owned(),
                delta: draft_2_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "revision-draft-call".to_owned(),
                name: sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned(),
                args_json: draft_2_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "design the coordinator migration before touching anything".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::PlanRunFinished { .. })
    })?;
    let WorkerMessage::PlanRunFinished { entries, .. } = finished else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    let draft_1 = entries
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft)) => Some(draft.clone()),
            _ => None,
        })
        .expect("first plan review must commit a typed draft");
    assert_eq!(draft_1.summary, "Migrate the coordinator");

    // Revise runs a supervised second plan review: the worker owns the run, restores the
    // session, and surfaces the new draft through PlanRunFinished like any plan review.
    worker.send(WorkerCommand::RevisePlan {
        plan_id: draft_1.plan_id.as_str().to_owned(),
        expected_plan_hash: draft_1.plan_hash.clone(),
    })?;
    let guidance = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::UserInputRequested { .. })
    })?;
    let WorkerMessage::UserInputRequested { request, .. } = guidance else {
        unreachable!("recv_until only returns UserInputRequested");
    };
    worker.send(WorkerCommand::SubmitUserInputDecision {
        command_id: Some("revision-guidance-e2e".to_owned()),
        request_id: request.identity.request_id.as_str().to_owned(),
        generation: request.identity.generation,
        expected_request_hash: request.request_hash,
        decision: sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "revision_guidance".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "Preserve the public API and split the migration step.".to_owned(),
                },
            }],
        },
    })?;
    let started = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::PlanRunStarted { .. })
    })?;
    let WorkerMessage::PlanRunStarted { prompt } = started else {
        unreachable!("recv_until only returns PlanRunStarted");
    };
    assert!(prompt.contains("plan revision"));
    let revised = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::PlanRunFinished { .. })
    })?;
    let WorkerMessage::PlanRunFinished { entries, result } = revised else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    let drafts = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft)) => Some(draft.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(drafts.len(), 2, "the restored session carries both drafts");
    assert!(
        drafts
            .iter()
            .any(|draft| draft.summary == "Revised coordinator migration"),
        "the revision draft is committed into the returned session"
    );
    assert!(
        drafts
            .iter()
            .any(|draft| draft.summary == "Migrate the coordinator"),
        "the original draft is preserved in the returned session"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(decision))
                    if decision.decision == sigil_kernel::PlanDecision::RevisionRequested
            ))
            .count(),
        1,
        "the RevisionRequested decision is durable in the returned session"
    );
    let revised_draft_plan_id = drafts
        .iter()
        .find(|draft| draft.summary == "Revised coordinator migration")
        .map(|draft| draft.plan_id.as_str())
        .unwrap_or("");
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
                if attempt.status == sigil_kernel::PlanReviewAttemptStatus::DraftReady
                    && attempt.plan_id.as_str() == revised_draft_plan_id
        )),
        "the revision attempt terminates as DraftReady in the returned session"
    );
    let revision_attempt = entries
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
                if attempt.status == sigil_kernel::PlanReviewAttemptStatus::DraftReady
                    && attempt.plan_id.as_str() == revised_draft_plan_id =>
            {
                Some(attempt)
            }
            _ => None,
        })
        .expect("actual finalized revision attempt");
    let records = sigil_kernel::JsonlSessionStore::read_event_records(&session_log_path)?;
    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    let run_id = sigil_kernel::plan_review_revision_run_id(revision_attempt);
    let terminal = projection
        .events_in_order()
        .into_iter()
        .filter(|entry| entry.run_id == run_id)
        .collect::<Vec<_>>();
    assert_eq!(
        terminal.len(),
        1,
        "the real TUI revision commits exactly one terminal outbox"
    );
    assert!(
        matches!(&terminal[0].event.event,
            sigil_kernel::PublicRunEventKind::RunFinished { final_text }
                if final_text == &result.final_text
        ),
        "the worker must surface the original durable public result"
    );
    assert!(
        records.iter().any(
            |record| record.stored_event().event_id == terminal[0].domain_event_id
                && record.stored_event().event_kind()
                    == Some(sigil_kernel::DurableEventType::PlanReviewAttempt)
        ),
        "revision outbox must bind the actual attempt, not a fabricated root run"
    );
    worker.shutdown()?;
    Ok(())
}
