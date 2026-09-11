use std::{collections::BTreeSet, fs, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use sigil_kernel::{
    Agent, AgentRole, AgentRunInput, AgentRunPurpose, CONTINUE_EXISTING_TASK_TOOL_NAME,
    CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME, ControlEntry, ConversationInputKind,
    ConversationInputQueueId, ConversationInputQueuedEntry, ConversationInputStatus,
    ConversationInputTarget, JsonlSessionStore, ModelMessage, MultiAgentMode,
    PlanArtifactProjection, PlanTaskStartMode, ProviderChunk, ReasoningEffort, Session,
    SessionLogEntry, SessionRef, TASK_GUIDANCE_APPLY_TOOL_NAME, TaskAdmissionReason,
    TaskAdmissionTrigger, TaskHandoffRequestedEntry, TaskId, TaskIsolationMode, TaskPauseRequest,
    TaskPlanEntry, TaskPlanStatus, TaskRoutingPolicy, TaskRunEntry, TaskRunStatus, TaskStepId,
    TaskStepMode, TaskStepSpec, TaskStepStatus, Tool, ToolAccess, ToolCall, ToolCategory,
    ToolContext, ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta, ToolSpec,
    project_conversation_prompt_for_persistence,
};
use tempfile::tempdir;

use super::{
    super::{WorkerCommand, WorkerMessage},
    common::{
        PlannedProvider, StreamPlan, TestWorker, failing_role_provider_builder,
        planned_role_provider_builder, planned_role_provider_builder_with_stream_start_signal,
        routed_session_identity, routed_test_root_config, routed_unauthenticated_test_root_config,
        spawn_test_worker, spawn_test_worker_with_existing_authority_composition,
        spawn_test_worker_with_role_provider_builder, submit_plan_review_result_chunks,
        test_root_config, wait_for_session_entry,
    },
};

struct PlannerDiscoveryReadTool;

#[async_trait]
impl Tool for PlannerDiscoveryReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".to_owned(),
            description: "Read one workspace file during planner discovery tests.".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "read_file",
            "read contents",
            ToolResultMeta::default(),
        ))
    }
}

fn task_workspace_read_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(PlannerDiscoveryReadTool));
    registry
}

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
        "prompt": "Choose the migration boundary",
        "questions": [{
            "id": "scope",
            "header": "Scope",
            "question": "Which module should be migrated first?",
            "required": true,
            "field": {"kind": "text", "multiline": false, "max_chars": 120}
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
fn ordinary_chat_auto_handoff_runs_durable_task_under_the_same_worker_run() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-auto-task-handoff-e2e.jsonl");
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    initialize_routed_plan_review_session(&root_config, &session_log_path, "planned-model")?;
    let handoff_args = r#"{"reason_codes":["cross_layer","long_verification"]}"#;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::ToolCallStart {
            id: "handoff-call".to_owned(),
            name: "request_task_planning".to_owned(),
        },
        ProviderChunk::ToolCallArgsDelta {
            id: "handoff-call".to_owned(),
            delta: handoff_args.to_owned(),
        },
        ProviderChunk::ToolCallComplete(ToolCall {
            id: "handoff-call".to_owned(),
            name: "request_task_planning".to_owned(),
            args_json: handoff_args.to_owned(),
        }),
        ProviderChunk::Done,
    ])]);
    let task_plan_args = r#"{
        "plan_version": 1,
        "status": "accepted",
        "steps": [{
            "step_id": "inspect_runtime",
            "title": "Inspect runtime handoff",
            "role": "executor",
            "mode": "read",
            "isolation": "shared_read_only"
        }]
    }"#;
    let role_provider_builder = planned_role_provider_builder(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "task-plan-call".to_owned(),
                name: "task_plan_update".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "task-plan-call".to_owned(),
                delta: task_plan_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "task-plan-call".to_owned(),
                name: "task_plan_update".to_owned(),
                args_json: task_plan_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("durable task completed".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("durable task synthesis completed".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config.clone(),
        session_log_path.clone(),
        Agent::new(provider, task_workspace_read_registry()),
        workspace_root.clone(),
        role_provider_builder,
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "inspect the runtime and verify the cross-layer handoff".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::TaskRunStarted { .. }))?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::TaskRunFinished { .. })
    })?;
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };
    assert_eq!(
        status,
        TaskRunStatus::Completed,
        "unexpected durable task entries: {entries:#?}"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::User(_)))
            .count(),
        1,
        "planner and executor prompts must remain transient"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(_))
            ))
            .count(),
        1
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskHandoffResolved(_))
            ))
            .count(),
        1
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(_))
            ))
            .count(),
        1,
        "automatic handoff must bind the task to its inherited root cancellation scope"
    );
    assert_finished_public_root(&session_log_path, &workspace_root, &root_config)?;
    worker.shutdown()?;
    Ok(())
}

#[test]
fn task_planner_question_resumes_under_the_same_supervised_tui_task() -> Result<()> {
    fn apply_until<T>(
        worker: &TestWorker,
        app: &mut crate::app::AppState,
        attention_dismissed: bool,
        mut select: impl FnMut(&WorkerMessage) -> Option<T>,
    ) -> Result<T> {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let message = worker
                .recv_with_timeout(deadline.saturating_duration_since(std::time::Instant::now()))?;
            let selected = select(&message);
            let debug = format!("{message:?}");
            app.handle_worker_message(message)?;
            if attention_dismissed {
                assert!(
                    app.pending_user_input().is_none(),
                    "input attention reopened after {debug}"
                );
                assert!(
                    app.composer.pending_user_input_queue.is_empty(),
                    "old input queued after {debug}"
                );
            }
            if let Some(selected) = selected {
                return Ok(selected);
            }
        }
    }

    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-task-planner-question-e2e.jsonl");
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let handoff_args = r#"{"reason_codes":["cross_layer","long_verification"]}"#;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::ToolCallStart {
            id: "question-handoff-call".to_owned(),
            name: "request_task_planning".to_owned(),
        },
        ProviderChunk::ToolCallArgsDelta {
            id: "question-handoff-call".to_owned(),
            delta: handoff_args.to_owned(),
        },
        ProviderChunk::ToolCallComplete(ToolCall {
            id: "question-handoff-call".to_owned(),
            name: "request_task_planning".to_owned(),
            args_json: handoff_args.to_owned(),
        }),
        ProviderChunk::Done,
    ])]);
    let question_args = r#"{
        "prompt": "Choose the subsystem to inspect",
        "questions": [{
            "id": "scope",
            "header": "Scope",
            "question": "Which subsystem should the task inspect?",
            "required": true,
            "field": {
                "kind": "text",
                "multiline": false,
                "max_chars": 128
            }
        }]
    }"#;
    let plan_args = r##"{
        "plan_version": 1,
        "status": "accepted",
        "steps": [{
            "step_id": "inspect_runtime",
            "title": "Inspect the selected runtime subsystem",
            "role": "executor",
            "mode": "read",
            "isolation": "shared_read_only"
        }]
    }"##;
    let continuation_gate = Arc::new(tokio::sync::Notify::new());
    let role_provider_builder = planned_role_provider_builder(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "task-planner-question-call".to_owned(),
                name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "task-planner-question-call".to_owned(),
                delta: question_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "task-planner-question-call".to_owned(),
                name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
                args_json: question_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::GatedChunks {
            gate: Arc::clone(&continuation_gate),
            chunks: vec![
                ProviderChunk::ToolCallStart {
                    id: "task-plan-after-answer".to_owned(),
                    name: sigil_kernel::TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
                },
                ProviderChunk::ToolCallArgsDelta {
                    id: "task-plan-after-answer".to_owned(),
                    delta: plan_args.to_owned(),
                },
                ProviderChunk::ToolCallComplete(ToolCall {
                    id: "task-plan-after-answer".to_owned(),
                    name: sigil_kernel::TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
                    args_json: plan_args.to_owned(),
                }),
                ProviderChunk::Done,
            ],
        },
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "readonly-bash-after-answer".to_owned(),
                name: "bash".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "readonly-bash-after-answer".to_owned(),
                delta: r#"{"command":"echo hello"}"#.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "readonly-bash-after-answer".to_owned(),
                name: "bash".to_owned(),
                args_json: r#"{"command":"echo hello"}"#.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("task step completed after clarification".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("task synthesis completed after clarification".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let mut app =
        crate::app::AppState::from_root_config(&workspace_root.join("sigil.toml"), &root_config);
    let (composition, authority_root) = super::common::test_authority_composition(&workspace_root)?;
    let paths = sigil_runtime::resolve_sigil_paths(
        &root_config.storage,
        &root_config.session,
        &workspace_root,
    );
    let mut registry = ToolRegistry::new();
    sigil_tools_builtin::register_builtin_tools_with_managed_execution_and_terminal_config(
        &mut registry,
        sigil_tools_builtin::BuiltinToolPaths::workspace_defaults(&workspace_root),
        composition.command_execution.clone(),
        sigil_tools_builtin::TerminalExecutionConfig::from_execution_config(&root_config.execution),
        None,
        Some(sigil_runtime::authority_scratch_control(paths.scratch_root)),
    );
    let worker = super::common::spawn_test_worker_with_role_provider_builder_and_authority(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, registry),
        workspace_root,
        role_provider_builder,
        composition,
        Some(authority_root),
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "inspect the selected subsystem and verify the handoff".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let entries = apply_until(&worker, &mut app, false, |message| match message {
        WorkerMessage::TaskRunFinished {
            status: TaskRunStatus::Paused,
            entries,
            ..
        } => Some(entries.clone()),
        _ => None,
    })?;
    let task_id = entries
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskRun(run)) => Some(run.task_id.clone()),
            _ => None,
        })
        .expect("paused task must remain projected");
    let route_id = entries
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::AgentUserInputRoute(route))
                if route.budget_scope_id == task_id =>
            {
                Some(route.route_id.clone())
            }
            _ => None,
        })
        .expect("paused planner task must retain its exact attention route");
    let request = apply_until(&worker, &mut app, false, |message| match message {
        WorkerMessage::UserInputRequested { request, .. } => Some(request.clone()),
        _ => None,
    })?;
    assert!(app.pending_user_input().is_some_and(|form| form.open));
    assert!(matches!(
        request.source,
        sigil_kernel::UserInputSourceV1::Planner { .. }
    ));

    worker.send(WorkerCommand::SubmitUserInputDecision {
        command_id: Some("task-planner-question-e2e-answer".to_owned()),
        request_id: request.identity.request_id.as_str().to_owned(),
        generation: request.identity.generation,
        expected_request_hash: request.request_hash.clone(),
        decision: sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "runtime".to_owned(),
                },
            }],
        },
    })?;
    apply_until(&worker, &mut app, false, |message| match message {
        WorkerMessage::UserInputDecisionApplied {
            request: applied,
            continuation_started,
            ..
        } if applied.identity == request.identity => {
            assert!(*continuation_started);
            Some(())
        }
        _ => None,
    })?;
    assert!(
        app.pending_user_input().is_none(),
        "the accepted answer must close Input required and Resume"
    );
    assert!(app.composer.pending_user_input_queue.is_empty());
    let resumed_task = apply_until(&worker, &mut app, true, |message| match message {
        WorkerMessage::TaskRunStarted { task_id, .. } => Some(task_id.clone()),
        _ => None,
    })?;
    assert_eq!(resumed_task, task_id.as_str());
    assert!(
        app.runtime.is_busy,
        "the continued planner must be running while its stream is gated"
    );
    app.composer.input = "follow-up after the accepted answer".to_owned();
    assert!(
        matches!(
            app.submit_input()?,
            Some(crate::app::AppAction::QueueConversationInput { prompt, .. })
                if prompt == "follow-up after the accepted answer"
        ),
        "the retired input form must not block follow-up admission"
    );
    assert!(app.composer.input.is_empty());
    continuation_gate.notify_one();

    let mut saw_bash_result = false;
    let entries = apply_until(&worker, &mut app, true, |message| {
        let event = match message {
            WorkerMessage::Event(event) | WorkerMessage::AgentThreadEvent { event, .. } => {
                Some(event.as_ref())
            }
            _ => None,
        };
        if let Some(sigil_kernel::RunEvent::ToolResult(result)) = event
            && result.call_id == "readonly-bash-after-answer"
        {
            assert_eq!(result.tool_name, "bash");
            assert!(!result.is_error(), "builtin bash must succeed: {result:?}");
            assert!(result.content.contains("hello"));
            saw_bash_result = true;
        }
        match message {
            WorkerMessage::TaskRunFinished {
                status, entries, ..
            } => {
                assert_eq!(
                    *status,
                    TaskRunStatus::Completed,
                    "continued task failed: {entries:#?}"
                );
                Some(entries.clone())
            }
            _ => None,
        }
    })?;
    assert!(
        saw_bash_result,
        "the read-only step must execute real builtin bash echo hello"
    );
    let projection = sigil_kernel::AgentUserInputRouteProjectionV1::from_session_entries(&entries)?;
    assert_eq!(projection.pending().count(), 0);
    assert_eq!(
        projection.route(&route_id).map(|route| route.status),
        Some(sigil_kernel::AgentRouteStatus::Resolved)
    );
    let task = Session::load_from_store(
        "planned",
        "planned-model",
        JsonlSessionStore::new(&session_log_path)?,
    )?
    .task_state_projection()
    .tasks
    .get(&task_id)
    .cloned()
    .expect("completed task must remain durable");
    let planner_attempts = task
        .participant_attempts_for(sigil_kernel::TaskParticipantPurpose::Planner, None, None)
        .into_iter()
        .collect::<Vec<_>>();
    assert_eq!(planner_attempts.len(), 1);
    assert_eq!(
        planner_attempts[0].status,
        sigil_kernel::TaskParticipantAttemptStatus::Completed
    );
    worker.shutdown()?;
    Ok(())
}

#[test]
fn automatic_handoff_task_can_pause_stop_and_resume_on_its_inherited_run_scope() -> Result<()> {
    let worker_timeout = Duration::from_secs(30);
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-auto-task-pause-resume-e2e.jsonl");
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let handoff_args = r#"{"reason_codes":["cross_layer","long_verification"]}"#;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::ToolCallStart {
            id: "handoff-call".to_owned(),
            name: "request_task_planning".to_owned(),
        },
        ProviderChunk::ToolCallArgsDelta {
            id: "handoff-call".to_owned(),
            delta: handoff_args.to_owned(),
        },
        ProviderChunk::ToolCallComplete(ToolCall {
            id: "handoff-call".to_owned(),
            name: "request_task_planning".to_owned(),
            args_json: handoff_args.to_owned(),
        }),
        ProviderChunk::Done,
    ])]);
    let task_plan_args = r#"{
        "plan_version": 1,
        "status": "accepted",
        "steps": [{
            "step_id": "inspect_runtime",
            "title": "Inspect runtime handoff",
            "role": "executor",
            "mode": "read",
            "isolation": "shared_read_only"
        }]
    }"#;
    let (role_provider_builder, role_stream_started_rx) =
        planned_role_provider_builder_with_stream_start_signal(vec![
            StreamPlan::Chunks(vec![
                ProviderChunk::ToolCallStart {
                    id: "task-plan-call".to_owned(),
                    name: "task_plan_update".to_owned(),
                },
                ProviderChunk::ToolCallArgsDelta {
                    id: "task-plan-call".to_owned(),
                    delta: task_plan_args.to_owned(),
                },
                ProviderChunk::ToolCallComplete(ToolCall {
                    id: "task-plan-call".to_owned(),
                    name: "task_plan_update".to_owned(),
                    args_json: task_plan_args.to_owned(),
                }),
                ProviderChunk::Done,
            ]),
            StreamPlan::Pending,
            StreamPlan::Pending,
            StreamPlan::Chunks(vec![
                ProviderChunk::TextDelta("resumed task completed".to_owned()),
                ProviderChunk::Done,
            ]),
            StreamPlan::Chunks(vec![
                ProviderChunk::TextDelta("resumed task synthesis completed".to_owned()),
                ProviderChunk::Done,
            ]),
        ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, task_workspace_read_registry()),
        workspace_root,
        role_provider_builder,
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "inspect the runtime, pause safely, then resume".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::RunStarted { .. })
        })
        .context("waiting for root run start before automatic handoff")?;
    let started = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::TaskRunStarted { .. })
        })
        .context("waiting for automatic durable task start")?;
    let WorkerMessage::TaskRunStarted { task_id, .. } = started else {
        unreachable!("recv_until only returns TaskRunStarted");
    };
    let expected_task_id = TaskId::new(task_id)?;
    role_stream_started_rx
        .recv_timeout(worker_timeout)
        .context("waiting for task planner provider stream to start")?;
    role_stream_started_rx
        .recv_timeout(worker_timeout)
        .context("waiting for task executor provider stream to start")?;
    wait_for_session_entry(&session_log_path, |entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskStep(step))
                if step.task_id == expected_task_id && step.status == TaskStepStatus::Running
        )
    })
    .context("waiting for the task executor step to become durable and running")?;

    worker.send(WorkerCommand::PauseTask {
        request: TaskPauseRequest::new(expected_task_id.clone(), 1),
    })?;
    let requested = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::TaskPauseRequested { .. })
        })
        .context("waiting for exact task pause acknowledgement")?;
    assert!(matches!(
        requested,
        WorkerMessage::TaskPauseRequested { ref task_id }
            if task_id == expected_task_id.as_str()
    ));
    let paused = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::TaskRunPaused { .. })
        })
        .context("waiting for quiescent durable task pause")?;
    let WorkerMessage::TaskRunPaused {
        task_id: paused_task_id,
        entries,
        ..
    } = paused
    else {
        unreachable!("recv_until only returns TaskRunPaused");
    };
    assert_eq!(paused_task_id, expected_task_id.as_str());
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskRun(task))
            if task.task_id == expected_task_id && task.status == TaskRunStatus::Paused
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskStep(step))
            if step.task_id == expected_task_id && step.status == TaskStepStatus::Interrupted
    )));

    worker.send(WorkerCommand::ContinueTask {
        task_id: Some(expected_task_id.as_str().to_owned()),
        guidance: None,
    })?;
    let _ = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::TaskRunStarted { .. })
        })
        .context("waiting for paused task to resume")?;
    role_stream_started_rx
        .recv_timeout(worker_timeout)
        .context("waiting for resumed task executor stream to start")?;
    wait_for_session_entry(&session_log_path, |entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskStep(step))
                if step.task_id == expected_task_id && step.status == TaskStepStatus::Running
        )
    })
    .context("waiting for the resumed task step to become running")?;

    worker.send(WorkerCommand::CancelRun)?;
    let interrupted = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::RunInterrupted { .. })
        })
        .context("waiting for stopped task run to remain resumable")?;
    let WorkerMessage::RunInterrupted { entries, .. } = interrupted else {
        unreachable!("recv_until only returns RunInterrupted");
    };
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskRun(task))
            if task.task_id == expected_task_id && task.status == TaskRunStatus::Interrupted
    )));
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskRun(task))
            if task.task_id == expected_task_id && task.status == TaskRunStatus::Cancelled
    )));

    worker.send(WorkerCommand::ContinueTask {
        task_id: Some(expected_task_id.as_str().to_owned()),
        guidance: None,
    })?;
    let _ = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::TaskRunStarted { .. })
        })
        .context("waiting for interrupted task to resume")?;
    let finished = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::TaskRunFinished { .. })
        })
        .map_err(|error| {
            let entries = JsonlSessionStore::read_entries(&session_log_path).unwrap_or_default();
            anyhow!(
                "waiting for resumed task completion: {error:#}; durable entries: {}",
                control_entry_debug(&entries)
            )
        })?;
    assert!(matches!(
        finished,
        WorkerMessage::TaskRunFinished {
            ref task_id,
            status: TaskRunStatus::Completed,
            ..
        } if task_id == expected_task_id.as_str()
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn queued_task_guidance_promotes_at_idle_safe_point_and_continues_exact_task() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-task-guidance-e2e.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    sigil_runtime::bind_session_composition(&mut session, &root_config)?;
    let task_id = TaskId::new("task_guidance_e2e")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative(
            session_log_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("session.jsonl"),
        )?,
        objective: "finish the recovered task".to_owned(),
        title: None,

        status: TaskRunStatus::Paused,
        reason: Some("waiting at a scheduler safe point".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("finish")?,
            title: "Finish the pending work".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: Some(TaskStepMode::Read),
            isolation: Some(TaskIsolationMode::SharedReadOnly),
        }],
        reason: None,
    }))?;
    let queue_id = ConversationInputQueueId::new("queue_task_guidance_e2e")?;
    let guidance = project_conversation_prompt_for_persistence("prioritize the restart edge");
    session.append_control(ControlEntry::ConversationInputQueued(
        ConversationInputQueuedEntry {
            queue_id: queue_id.clone(),
            target: ConversationInputTarget::Task {
                task_id: task_id.clone(),
            },
            kind: ConversationInputKind::TaskGuidance,
            prompt_hash: guidance.prompt_hash,
            prompt: guidance.safe_prompt,
            reasoning_effort: Some(ReasoningEffort::High),
            created_at_ms: Some(1),
        },
    ))?;
    drop(session);

    let role_provider_builder = planned_role_provider_builder(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "call-task-guidance-apply".to_owned(),
                name: TASK_GUIDANCE_APPLY_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "call-task-guidance-apply".to_owned(),
                delta: r#"{"reason":"prioritizes_pending_step","target_step_ids":["finish"]}"#
                    .to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-task-guidance-apply".to_owned(),
                name: TASK_GUIDANCE_APPLY_TOOL_NAME.to_owned(),
                args_json: r#"{"reason":"prioritizes_pending_step","target_step_ids":["finish"]}"#
                    .to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("guided step completed".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("guided task synthesis completed".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path,
        Agent::new(
            PlannedProvider::new(Vec::new()),
            task_workspace_read_registry(),
        ),
        workspace_root,
        role_provider_builder,
    )?;

    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::TaskRunStarted { .. }))?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::TaskRunFinished { .. })
    })?;
    let WorkerMessage::TaskRunFinished {
        task_id: finished_task_id,
        status,
        entries,
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };
    assert_eq!(finished_task_id, task_id.as_str());
    assert_eq!(status, TaskRunStatus::Completed);
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskGuidancePromoted(promoted))
                    if promoted.queue_id == queue_id && promoted.task_id == task_id
            ))
            .count(),
        1
    );
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskGuidanceApplied(applied))
            if applied.queue_id == queue_id
                && applied.task_id == task_id
                && applied.target_step_ids == vec![TaskStepId::new("finish").expect("valid step id")]
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(changed))
            if changed.queue_id == queue_id && changed.status == ConversationInputStatus::Delivered
    )));
    assert!(
        entries
            .iter()
            .all(|entry| !matches!(entry, SessionLogEntry::User(_))),
        "task guidance must remain transient instead of entering parent user history"
    );
    assert!(entries.iter().all(|entry| !matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskRun(run))
            if run.reason.as_deref().is_some_and(|reason| reason.contains("prioritize the restart edge"))
    )));
    worker.shutdown()?;
    Ok(())
}

#[derive(Clone, Copy)]
enum TypedTaskContinuationDispatch {
    Direct,
    QueuedRunNext,
}

fn run_typed_task_continuation_from_conversation(
    dispatch: TypedTaskContinuationDispatch,
) -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-typed-task-continuation-e2e.jsonl");
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    sigil_runtime::bind_session_composition(&mut session, &root_config)?;
    let task_id = TaskId::new("typed_task_continuation_e2e")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative(
            session_log_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("session.jsonl"),
        )?,
        objective: "finish the current durable task".to_owned(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: Some("waiting for a semantic follow-up".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("finish")?,
            title: "Finish the pending work".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: Some(TaskStepMode::Read),
            isolation: Some(TaskIsolationMode::SharedReadOnly),
        }],
        reason: None,
    }))?;
    drop(session);

    let exact_guidance = "finish the task we were already working on";
    let continue_args =
        r#"{"reason":"continue_current_task","action":"apply_current_request_as_guidance"}"#;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::ToolCallStart {
            id: "continue-existing-task".to_owned(),
            name: CONTINUE_EXISTING_TASK_TOOL_NAME.to_owned(),
        },
        ProviderChunk::ToolCallArgsDelta {
            id: "continue-existing-task".to_owned(),
            delta: continue_args.to_owned(),
        },
        ProviderChunk::ToolCallComplete(ToolCall {
            id: "continue-existing-task".to_owned(),
            name: CONTINUE_EXISTING_TASK_TOOL_NAME.to_owned(),
            args_json: continue_args.to_owned(),
        }),
        ProviderChunk::Done,
    ])]);
    let guidance_args = r#"{"reason":"prioritizes_pending_step","target_step_ids":["finish"]}"#;
    let role_provider_builder = planned_role_provider_builder(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "apply-conversation-guidance".to_owned(),
                name: TASK_GUIDANCE_APPLY_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "apply-conversation-guidance".to_owned(),
                delta: guidance_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "apply-conversation-guidance".to_owned(),
                name: TASK_GUIDANCE_APPLY_TOOL_NAME.to_owned(),
                args_json: guidance_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("continued task step completed".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("continued task synthesis completed".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
        role_provider_builder,
    )?;

    match dispatch {
        TypedTaskContinuationDispatch::Direct => {
            worker.send(WorkerCommand::SubmitPrompt {
                prompt: exact_guidance.to_owned(),
                reasoning_effort: ReasoningEffort::High,
            })?;
            let _ = worker
                .recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))
                .context("direct conversation did not start")?;
        }
        TypedTaskContinuationDispatch::QueuedRunNext => {
            worker.send(WorkerCommand::SetConversationQueuePaused { paused: true })?;
            let _ = worker
                .recv_until(|message| {
                    matches!(
                        message,
                        WorkerMessage::ConversationQueueUpdated { paused: true, .. }
                    )
                })
                .context("conversation queue did not pause")?;
            worker.send(WorkerCommand::QueueConversationInput {
                prompt: exact_guidance.to_owned(),
                kind: ConversationInputKind::Chat,
                target: ConversationInputTarget::MainThread,
                reasoning_effort: ReasoningEffort::High,
            })?;
            let queued = worker
                .recv_until(|message| {
                    matches!(
                        message,
                        WorkerMessage::ConversationQueueUpdated {
                            items,
                            paused: true,
                            ..
                        } if items.len() == 1 && items[0].queued.prompt == exact_guidance
                    )
                })
                .context("conversation follow-up was not durably queued")?;
            let queue_id = match queued {
                WorkerMessage::ConversationQueueUpdated { items, .. } => {
                    items[0].queued.queue_id.clone()
                }
                _ => unreachable!("queue predicate guarantees an update"),
            };
            worker.send(WorkerCommand::PromoteQueuedConversationInput { queue_id })?;
            let _ = worker
                .recv_until_with_timeout(Duration::from_secs(10), |message| {
                    matches!(
                        message,
                        WorkerMessage::ConversationQueueDispatchStarted { prompt, .. }
                            if prompt == exact_guidance
                    )
                })
                .context("Run next did not dispatch the typed Task continuation")?;
        }
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let started = loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("typed Task continuation did not start");
        }
        let message = worker.recv_with_timeout(remaining)?;
        match message {
            WorkerMessage::TaskRunStarted {
                task_id: ref started_task_id,
                ..
            } if started_task_id == task_id.as_str() => break message,
            WorkerMessage::RunFailed(error) => {
                let entries = JsonlSessionStore::read_entries(&session_log_path)?;
                anyhow::bail!(
                    "typed Task continuation failed before start: {error}; durable entries: {}",
                    control_entry_debug(&entries)
                );
            }
            _ => {}
        }
    };
    assert!(matches!(started, WorkerMessage::TaskRunStarted { .. }));
    let finished = worker
        .recv_until_with_timeout(Duration::from_secs(10), |message| {
            matches!(
                message,
                WorkerMessage::TaskRunFinished { task_id: finished, .. }
                    if finished == task_id.as_str()
            )
        })
        .context("typed Task continuation did not reach a terminal state")?;
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };
    assert_eq!(status, TaskRunStatus::Completed);
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(selected))
            if selected.task_id == task_id && selected.guidance == exact_guidance
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskGuidanceApplied(applied))
            if applied.task_id == task_id
                && applied.target_step_ids
                    == vec![TaskStepId::new("finish").expect("valid step id")]
    )));
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(bound))
                    if bound.task_id == task_id
            ))
            .count(),
        1
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::User(message)
                    if message.content.as_deref() == Some(exact_guidance)
            ))
            .count(),
        1,
        "conversation continuation guidance must enter parent history exactly once"
    );

    worker.shutdown()?;
    Ok(())
}

#[test]
fn direct_conversation_continues_exact_current_task_with_guidance_review() -> Result<()> {
    run_typed_task_continuation_from_conversation(TypedTaskContinuationDispatch::Direct)
}

#[test]
fn queued_run_next_continues_exact_current_task_with_guidance_review() -> Result<()> {
    run_typed_task_continuation_from_conversation(TypedTaskContinuationDispatch::QueuedRunNext)
}

#[derive(Clone, Copy)]
enum ExplicitTaskContinuationPlan {
    Accepted,
    Missing,
}

fn run_explicit_task_continuation_after_user_clear(
    plan: ExplicitTaskContinuationPlan,
) -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let suffix = match plan {
        ExplicitTaskContinuationPlan::Accepted => "accepted-plan",
        ExplicitTaskContinuationPlan::Missing => "missing-plan",
    };
    let session_log_path = temp.path().join(format!(
        ".sigil/sessions/session-explicit-continue-{suffix}.jsonl"
    ));
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    sigil_runtime::bind_session_composition(&mut session, &root_config)?;
    let task_id = TaskId::new(format!("explicit_continue_{suffix}"))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative(
            session_log_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("session.jsonl"),
        )?,
        objective: "resume the exact recovered task".to_owned(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: Some("waiting for an explicit continuation".to_owned()),
    }))?;
    if matches!(plan, ExplicitTaskContinuationPlan::Accepted) {
        session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: TaskStepId::new("finish")?,
                title: "Finish the recovered work".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: Some(TaskStepMode::Read),
                isolation: Some(TaskIsolationMode::SharedReadOnly),
            }],
            reason: None,
        }))?;
    }
    session.append_user_message(ModelMessage::user("explain an unrelated module first"))?;
    assert!(
        session.task_state_projection().current_task().is_none(),
        "the ordinary User turn must clear Task focus before /task continue"
    );
    drop(session);

    let mut role_plans = Vec::new();
    if matches!(plan, ExplicitTaskContinuationPlan::Missing) {
        let task_plan_args = r#"{
            "plan_version": 1,
            "status": "accepted",
            "steps": [{
                "step_id": "finish",
                "title": "Finish the recovered work",
                "role": "executor",
                "mode": "read",
                "isolation": "shared_read_only"
            }]
        }"#;
        role_plans.push(StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "recover-task-plan".to_owned(),
                name: "task_plan_update".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "recover-task-plan".to_owned(),
                delta: task_plan_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "recover-task-plan".to_owned(),
                name: "task_plan_update".to_owned(),
                args_json: task_plan_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]));
    }
    role_plans.extend([
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("explicit continuation step completed".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("explicit continuation synthesis completed".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path,
        Agent::new(
            PlannedProvider::new(Vec::new()),
            task_workspace_read_registry(),
        ),
        workspace_root,
        planned_role_provider_builder(role_plans),
    )?;

    worker.send(WorkerCommand::ContinueTask {
        task_id: None,
        guidance: None,
    })?;
    let _ = worker
        .recv_until_with_timeout(Duration::from_secs(10), |message| {
            matches!(
                message,
                WorkerMessage::TaskRunStarted { task_id: started, .. }
                    if started == task_id.as_str()
            )
        })
        .context("explicit Task continuation did not start")?;
    let finished = worker
        .recv_until_with_timeout(Duration::from_secs(10), |message| {
            matches!(
                message,
                WorkerMessage::TaskRunFinished { task_id: finished, .. }
                    if finished == task_id.as_str()
            )
        })
        .context("explicit Task continuation did not finish")?;
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };
    assert_eq!(status, TaskRunStatus::Completed);
    assert_eq!(
        sigil_kernel::TaskStateProjection::from_entries(&entries)
            .current_task()
            .map(|task| &task.task_id),
        Some(&task_id)
    );
    let selections = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskRunTargetSelected(selected))
                if selected.task_id == task_id =>
            {
                Some(selected)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(selections.len(), 1);
    assert_eq!(selections[0].task_status, TaskRunStatus::Paused);
    assert_eq!(
        selections[0].plan_version,
        match plan {
            ExplicitTaskContinuationPlan::Accepted => Some(1),
            ExplicitTaskContinuationPlan::Missing => None,
        }
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::User(_)))
            .count(),
        1,
        "/task continue must not synthesize another parent User turn"
    );

    worker.shutdown()?;
    Ok(())
}

#[test]
fn explicit_continue_refocuses_paused_task_after_an_ordinary_user_turn() -> Result<()> {
    run_explicit_task_continuation_after_user_clear(ExplicitTaskContinuationPlan::Accepted)
}

#[test]
fn explicit_continue_replans_no_plan_task_through_the_shared_continuation_runtime() -> Result<()> {
    run_explicit_task_continuation_after_user_clear(ExplicitTaskContinuationPlan::Missing)
}

#[test]
fn run_next_resumes_paused_task_guidance_after_its_initial_wake_was_consumed() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-paused-task-guidance-run-next.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    sigil_runtime::bind_session_composition(&mut session, &root_config)?;
    let task_id = TaskId::new("paused_task_guidance_run_next")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative(
            session_log_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("session.jsonl"),
        )?,
        objective: "resume task guidance only after Run next".to_owned(),
        title: None,

        status: TaskRunStatus::Paused,
        reason: Some("waiting for user guidance".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("finish")?,
            title: "Finish the pending work".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: Some(TaskStepMode::Read),
            isolation: Some(TaskIsolationMode::SharedReadOnly),
        }],
        reason: None,
    }))?;
    let queue_id = ConversationInputQueueId::new("queue_paused_task_guidance_run_next")?;
    let guidance = project_conversation_prompt_for_persistence("apply this guidance next");
    session.append_control(ControlEntry::ConversationInputQueued(
        ConversationInputQueuedEntry {
            queue_id: queue_id.clone(),
            target: ConversationInputTarget::Task {
                task_id: task_id.clone(),
            },
            kind: ConversationInputKind::TaskGuidance,
            prompt_hash: guidance.prompt_hash,
            prompt: guidance.safe_prompt,
            reasoning_effort: Some(ReasoningEffort::High),
            created_at_ms: Some(1),
        },
    ))?;
    session.append_control(ControlEntry::ConversationInputQueueControl(
        sigil_kernel::ConversationInputQueueControlEntry {
            action: sigil_kernel::ConversationInputQueueControlAction::Pause,
            reason: Some("exercise Run next wake".to_owned()),
            updated_at_ms: Some(2),
        },
    ))?;
    drop(session);

    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path,
        Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
        workspace_root,
        planned_role_provider_builder(vec![StreamPlan::Pending]),
    )?;

    let _ = worker.recv_until_with_timeout(Duration::from_secs(3), |message| {
        matches!(message, WorkerMessage::Notice(notice) if notice.contains("task guidance is waiting"))
    })?;
    worker.send(WorkerCommand::PromoteQueuedConversationInput { queue_id })?;
    let started = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::TaskRunStarted { task_id: started, .. }
            if started == task_id.as_str())
    })?;
    assert!(matches!(started, WorkerMessage::TaskRunStarted { .. }));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn auto_handoff_preflight_failure_pauses_task_with_recovery_state() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-auto-task-preflight-failure.jsonl");
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let handoff_args = r#"{"reason_codes":["cross_layer"]}"#;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::ToolCallStart {
            id: "handoff-preflight-failure".to_owned(),
            name: "request_task_planning".to_owned(),
        },
        ProviderChunk::ToolCallArgsDelta {
            id: "handoff-preflight-failure".to_owned(),
            delta: handoff_args.to_owned(),
        },
        ProviderChunk::ToolCallComplete(ToolCall {
            id: "handoff-preflight-failure".to_owned(),
            name: "request_task_planning".to_owned(),
            args_json: handoff_args.to_owned(),
        }),
        ProviderChunk::Done,
    ])]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path,
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
        failing_role_provider_builder(),
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "run a task whose role provider cannot be built".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::TaskRunFinished { .. })
    })?;
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };
    assert_eq!(status, TaskRunStatus::Paused);
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskRun(run))
            if run.status == TaskRunStatus::Paused
    )));
    worker.shutdown()?;
    Ok(())
}

#[test]
fn ordinary_simple_chat_in_auto_mode_remains_a_chat_without_task_admission() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-auto-simple-chat-e2e.jsonl");
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let direct_args = r#"{"reason":"does_not_meet_task_planning_criteria"}"#;
    let provider = PlannedProvider::new(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "direct-routing-call".to_owned(),
                name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "direct-routing-call".to_owned(),
                delta: direct_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "direct-routing-call".to_owned(),
                name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
                args_json: direct_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("A concise direct answer.".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path,
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
        planned_role_provider_builder(Vec::new()),
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "what does this symbol mean?".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    let WorkerMessage::RunFinished { entries, .. } = finished else {
        unreachable!("recv_until only returns RunFinished");
    };
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(
            ControlEntry::TaskHandoffRequested(_)
                | ControlEntry::TaskHandoffResolved(_)
                | ControlEntry::TaskRun(_)
        )
    )));
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Assistant(message)
                if message.content.as_deref() == Some("A concise direct answer.")
        )
    }));
    worker.shutdown()?;
    Ok(())
}

#[test]
fn startup_reconciles_requested_handoff_and_resumes_task_without_replaying_chat_provider()
-> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-auto-handoff-recovery-e2e.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    let parent_session_ref = SessionRef::new_relative(
        session_log_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("session.jsonl"),
    )?;
    let input = AgentRunInput::user("recover this cross-layer task");
    let bound = sigil_runtime::ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(sigil_runtime::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            task_executor_available: true,
        })
        .bind_conversation_input(
            &session,
            input,
            parent_session_ref,
            "foreground-run-crashed",
            None,
            31,
        )?;
    let AgentRunPurpose::Conversation(context) = bound.purpose.expect("conversation purpose")
    else {
        panic!("expected conversation purpose");
    };
    let binding = context.task_handoff.expect("auto handoff binding");
    let mut source_message = ModelMessage::user("recover this cross-layer task");
    source_message.id = binding.source_turn.message_id.clone();
    session.append_user_message(source_message)?;
    session.append_control(ControlEntry::TaskHandoffRequested(
        TaskHandoffRequestedEntry {
            handoff_id: binding.handoff_id,
            source_turn: binding.source_turn,
            trigger: TaskAdmissionTrigger::ModelRequested,
            reason_codes: vec![TaskAdmissionReason::CrossLayer],
            recovery_objective: None,
            policy_snapshot_hash: binding.policy_snapshot_hash,
            requested_at_ms: binding.requested_at_ms,
        },
    ))?;
    let queue_id = ConversationInputQueueId::new("queue_before_recovered_handoff")?;
    let follow_up = project_conversation_prompt_for_persistence(
        "apply this follow-up before starting the recovered task",
    );
    session.append_control(ControlEntry::ConversationInputQueued(
        ConversationInputQueuedEntry {
            queue_id: queue_id.clone(),
            target: ConversationInputTarget::MainThread,
            kind: ConversationInputKind::Chat,
            prompt_hash: follow_up.prompt_hash,
            prompt: follow_up.safe_prompt,
            reasoning_effort: Some(ReasoningEffort::High),
            created_at_ms: Some(32),
        },
    ))?;
    drop(session);

    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let task_plan_args = r#"{
        "plan_version": 1,
        "status": "accepted",
        "steps": [{
            "step_id": "resume_recovered_task",
            "title": "Resume recovered task",
            "role": "executor",
            "mode": "read",
            "isolation": "shared_read_only"
        }]
    }"#;
    let role_provider_builder = planned_role_provider_builder(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "recovered-task-plan-call".to_owned(),
                name: "task_plan_update".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "recovered-task-plan-call".to_owned(),
                delta: task_plan_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "recovered-task-plan-call".to_owned(),
                name: "task_plan_update".to_owned(),
                args_json: task_plan_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("recovered task completed".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("recovered task synthesis completed".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let direct_args = r#"{"reason":"does_not_meet_task_planning_criteria"}"#;
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path,
        Agent::new(
            PlannedProvider::new(vec![
                StreamPlan::Chunks(vec![
                    ProviderChunk::ToolCallStart {
                        id: "recovered-follow-up-route".to_owned(),
                        name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
                    },
                    ProviderChunk::ToolCallArgsDelta {
                        id: "recovered-follow-up-route".to_owned(),
                        delta: direct_args.to_owned(),
                    },
                    ProviderChunk::ToolCallComplete(ToolCall {
                        id: "recovered-follow-up-route".to_owned(),
                        name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
                        args_json: direct_args.to_owned(),
                    }),
                    ProviderChunk::Done,
                ]),
                StreamPlan::Chunks(vec![
                    ProviderChunk::TextDelta("follow-up handled first".to_owned()),
                    ProviderChunk::Done,
                ]),
            ]),
            task_workspace_read_registry(),
        ),
        workspace_root,
        role_provider_builder,
    )?;

    let dispatched = worker
        .recv_until_with_timeout(Duration::from_secs(10), |message| {
            matches!(
                message,
                WorkerMessage::ConversationQueueDispatchStarted { .. }
            )
        })
        .context("waiting for queued follow-up dispatch during startup recovery")?;
    assert!(matches!(
        dispatched,
        WorkerMessage::ConversationQueueDispatchStarted { queue_id: dispatched, .. }
            if dispatched == queue_id
    ));
    let _ = worker
        .recv_until_with_timeout(Duration::from_secs(10), |message| {
            matches!(message, WorkerMessage::TaskRunStarted { .. })
        })
        .context("waiting for recovered task to start after queued follow-up")?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::TaskRunFinished { .. })
    })?;
    assert!(matches!(
        finished,
        WorkerMessage::TaskRunFinished {
            status: TaskRunStatus::Completed,
            ..
        }
    ));
    worker.shutdown()?;
    Ok(())
}

#[test]
fn explicit_task_command_uses_typed_handoff_admission_before_planning() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-explicit-task-handoff-e2e.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let task_plan_args = r#"{
        "plan_version": 1,
        "status": "accepted",
        "steps": [{
            "step_id": "execute_explicit_task",
            "title": "Execute explicit task",
            "role": "executor",
            "mode": "read",
            "isolation": "shared_read_only"
        }]
    }"#;
    let role_provider_builder = planned_role_provider_builder(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "explicit-task-plan-call".to_owned(),
                name: "task_plan_update".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "explicit-task-plan-call".to_owned(),
                delta: task_plan_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "explicit-task-plan-call".to_owned(),
                name: "task_plan_update".to_owned(),
                args_json: task_plan_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("explicit task completed".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("explicit task synthesis completed".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path,
        Agent::new(
            PlannedProvider::new(Vec::new()),
            task_workspace_read_registry(),
        ),
        workspace_root,
        role_provider_builder,
    )?;

    worker.send(WorkerCommand::SubmitTask {
        prompt: "run the explicit durable task".to_owned(),
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::TaskRunStarted { .. }))?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::TaskRunFinished { .. })
    })?;
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };
    assert_eq!(status, TaskRunStatus::Completed);
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(request))
            if request.trigger == sigil_kernel::TaskAdmissionTrigger::ExplicitTaskCommand
    )));
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::User(_)))
            .count(),
        1
    );
    worker.shutdown()?;
    Ok(())
}

#[test]
fn explicit_task_planner_uses_configured_discovery_fanout_in_tui_runtime() -> Result<()> {
    assert_task_discovery_on_default_worker_stack(false, false)
}

#[test]
fn ordinary_chat_task_discovery_completes_on_default_worker_stack() -> Result<()> {
    assert_task_discovery_on_default_worker_stack(true, false)
}

#[test]
fn ordinary_chat_task_discovery_cancellation_settles_children_on_default_worker_stack() -> Result<()>
{
    assert_task_discovery_on_default_worker_stack(true, true)
}

fn assert_task_discovery_on_default_worker_stack(
    ordinary_chat: bool,
    cancel_during_discovery: bool,
) -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-planner-discovery-e2e.jsonl");
    let mut root_config = routed_unauthenticated_test_root_config(&workspace_root, "planned-model");
    if ordinary_chat {
        root_config.task.routing_policy = TaskRoutingPolicy::Auto;
        initialize_routed_plan_review_session(&root_config, &session_log_path, "planned-model")?;
    }
    let coordinator_plans = if ordinary_chat {
        vec![StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "discovery-handoff".to_owned(),
                name: "request_task_planning".to_owned(),
                args_json: r#"{"reason_codes":["cross_layer","long_verification"]}"#.to_owned(),
            }),
            ProviderChunk::Done,
        ])]
    } else {
        Vec::new()
    };
    let (provider, coordinator_streams) =
        PlannedProvider::new_with_stream_start_signal(coordinator_plans);
    root_config.task.multi_agent_mode = MultiAgentMode::ExplicitRequestOnly;
    root_config.task.max_planning_research_agents = 2;
    root_config.task.max_subagents = 4;
    let discovery_args = r#"{
        "probes": [
            {
                "probe_id": "kernel",
                "title": "Inspect kernel",
                "objective": "Inspect task contracts",
                "path_hints": ["crates/sigil-kernel"]
            },
            {
                "probe_id": "runtime",
                "title": "Inspect runtime",
                "objective": "Inspect orchestration wiring",
                "path_hints": ["crates/sigil-runtime"]
            }
        ]
    }"#;
    let task_plan_args = r#"{
        "plan_version": 1,
        "status": "accepted",
        "steps": [{
            "step_id": "execute_after_discovery",
            "title": "Execute after discovery",
            "role": "executor",
            "mode": "read",
            "isolation": "shared_read_only"
        }]
    }"#;
    let (role_provider_builder, role_streams) =
        planned_role_provider_builder_with_stream_start_signal(vec![
            StreamPlan::Chunks(vec![
                ProviderChunk::ToolCallComplete(ToolCall {
                    id: "planner-discovery-call".to_owned(),
                    name: sigil_runtime::REQUEST_TASK_DISCOVERY_TOOL_NAME.to_owned(),
                    args_json: discovery_args.to_owned(),
                }),
                ProviderChunk::Done,
            ]),
            if cancel_during_discovery {
                StreamPlan::Pending
            } else {
                StreamPlan::Chunks(vec![
                    ProviderChunk::TextDelta("kernel discovery complete".to_owned()),
                    ProviderChunk::Done,
                ])
            },
            if cancel_during_discovery {
                StreamPlan::Pending
            } else {
                StreamPlan::Chunks(vec![
                    ProviderChunk::TextDelta("runtime discovery complete".to_owned()),
                    ProviderChunk::Done,
                ])
            },
            StreamPlan::Chunks(vec![
                ProviderChunk::ToolCallComplete(ToolCall {
                    id: "task-plan-after-discovery".to_owned(),
                    name: "task_plan_update".to_owned(),
                    args_json: task_plan_args.to_owned(),
                }),
                ProviderChunk::Done,
            ]),
            StreamPlan::Chunks(vec![
                ProviderChunk::TextDelta("discovery-backed task completed".to_owned()),
                ProviderChunk::Done,
            ]),
            StreamPlan::Chunks(vec![
                ProviderChunk::TextDelta("discovery-backed synthesis completed".to_owned()),
                ProviderChunk::Done,
            ]),
        ]);
    let registry = task_workspace_read_registry();
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config.clone(),
        session_log_path.clone(),
        Agent::new(provider, registry),
        workspace_root.clone(),
        role_provider_builder,
    )?;

    // Exercise the production worker runtime without a test-only thread stack override. Ordinary
    // SubmitPrompt retains the outer coordinator frame while Planner polls both Explore children.
    let prompt = "inspect kernel and runtime before implementing".to_owned();
    if ordinary_chat {
        worker.send(WorkerCommand::SubmitPrompt {
            prompt,
            reasoning_effort: ReasoningEffort::Max,
        })?;
        let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    } else {
        worker.send(WorkerCommand::SubmitTask { prompt })?;
    }
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::TaskRunStarted { .. }))?;
    if cancel_during_discovery {
        for _ in 0..3 {
            role_streams
                .recv_timeout(Duration::from_secs(20))
                .context("waiting for Planner and both Explore provider streams")?;
        }
        worker.send(WorkerCommand::CancelRun)?;
        let cancelled = worker.recv_until_with_timeout(Duration::from_secs(20), |message| {
            matches!(
                message,
                WorkerMessage::RunCancelled { .. } | WorkerMessage::RunFailed(_)
            )
        })?;
        assert!(
            matches!(cancelled, WorkerMessage::RunCancelled { .. }),
            "discovery stop must close its foreground run: {cancelled:?}"
        );
        worker.shutdown()?;
        assert_eq!(coordinator_streams.try_iter().count(), 1);
        assert_eq!(
            role_streams.try_iter().count(),
            0,
            "cancellation must not dispatch another planner or executor request"
        );
        let entries = JsonlSessionStore::read_entries(&session_log_path)?;
        assert!(
            entries.iter().any(|entry| matches!(entry,
                SessionLogEntry::Control(ControlEntry::TaskRun(run))
                    if run.status == TaskRunStatus::Interrupted
            )),
            "unexpected stopped Task entries: {entries:#?}"
        );
        assert!(!entries.iter().any(|entry| matches!(entry,
            SessionLogEntry::Control(ControlEntry::TaskRun(run))
                if run.status == TaskRunStatus::Completed
        )));
        assert!(!entries.iter().any(|entry| matches!(entry,
            SessionLogEntry::Control(ControlEntry::TaskPlan(plan))
                if plan.status == TaskPlanStatus::Accepted
        )));
        let projection = sigil_kernel::AgentThreadStateProjection::from_entries(&entries);
        let explore_threads = projection
            .threads
            .values()
            .filter(|thread| {
                thread
                    .profile_id
                    .as_ref()
                    .is_some_and(|profile| profile.as_str() == sigil_runtime::EXPLORE_PROFILE_ID)
            })
            .collect::<Vec<_>>();
        assert_eq!(explore_threads.len(), 2);
        for thread in explore_threads {
            assert!(
                matches!(
                    thread.status,
                    sigil_kernel::AgentThreadStatus::Cancelled
                        | sigil_kernel::AgentThreadStatus::Interrupted
                ),
                "discovery child must be terminal after stop: {thread:?}"
            );
            assert_eq!(
                thread.duplicate_terminal_entries, 0,
                "discovery completion and foreground cancellation must not both close the child"
            );
            assert_eq!(thread.attempts.len(), 1);
            let attempt = thread
                .attempts
                .values()
                .next()
                .expect("one Explore attempt");
            assert!(
                attempt.interrupted.is_some(),
                "the stopped child must retain its attempt interruption audit"
            );
            assert_eq!(
                entries
                    .iter()
                    .filter(|entry| matches!(entry,
                        SessionLogEntry::Control(ControlEntry::AgentRunInterrupted(interrupted))
                            if interrupted.thread_id == thread.thread_id
                                && interrupted.attempt_id == attempt.attempt_id
                    ))
                    .count(),
                1,
                "each started Explore attempt must receive exactly one interruption audit"
            );
        }
        let records = JsonlSessionStore::read_event_records(&session_log_path)?;
        let finalized = records
            .iter()
            .map(sigil_kernel::SessionStreamRecord::stored_event)
            .find(|event| {
                event
                    .payload
                    .get("record")
                    .and_then(serde_json::Value::as_str)
                    == Some("finalized")
            })
            .context("discovery cancellation must durably record owner quiescence")?;
        assert_eq!(finalized.payload["outcome"], "cancelled");
        assert_eq!(finalized.payload["cleanup_complete"], true);
        assert_eq!(finalized.payload["active_effects"], 0);
        assert_eq!(finalized.payload["active_tasks"], 0);
        return Ok(());
    }
    let finished = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::TaskRunFinished { .. })
    })?;
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };

    assert_eq!(
        status,
        TaskRunStatus::Completed,
        "unexpected entries: {entries:#?}"
    );
    if ordinary_chat {
        assert_finished_public_root(&session_log_path, &workspace_root, &root_config)?;
    }
    worker.shutdown()?;
    assert_eq!(
        coordinator_streams.try_iter().count(),
        usize::from(ordinary_chat)
    );
    assert_eq!(
        role_streams.try_iter().count(),
        6,
        "unexpected planning or execution dispatch"
    );
    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry,
                SessionLogEntry::Control(ControlEntry::TaskPlan(plan))
                    if plan.status == TaskPlanStatus::Accepted
            ))
            .count(),
        1,
        "discovery must commit one plan without replanning"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(_))
            ))
            .count(),
        1
    );
    assert!(entries.iter().any(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::TaskRun(run))
            if run.status == TaskRunStatus::Completed
    )));
    let explore_threads = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::AgentThreadStarted(started))
                if started.profile_id.as_str() == sigil_runtime::EXPLORE_PROFILE_ID =>
            {
                Some(started.thread_id.clone())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(explore_threads.len(), 2);
    let completed_explore_threads = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::AgentThreadResultRecorded(result))
                if result.result.status == sigil_kernel::AgentThreadTerminalStatus::Completed
                    && explore_threads.contains(&result.result.thread_id) =>
            {
                Some(result.result.thread_id.clone())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(completed_explore_threads, explore_threads);
    Ok(())
}

#[test]
fn plan_handoff_run_now_uses_host_direct_execution_without_replanning() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-plan-handoff-e2e.jsonl");
    let root_config = routed_unauthenticated_test_root_config(&workspace_root, "planned-model");
    let draft_args = r##"{
  "schema_version": 1,
  "outcome": "draft",
  "content": "# Inspect approved README plan\n\n1. Inspect README.md\n2. Report whether the approved typo fix is needed\n\nPaths: README.md\n\nChecks: cargo test -p sigil-tui plan_handoff"
}"##;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(
        submit_plan_review_result_chunks("approved-plan-draft", draft_args),
    )]);
    let role_provider_builder = planned_role_provider_builder(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("approved plan inspection complete".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("approved plan report complete".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("approved plan complete".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let agent = Agent::new(provider, task_workspace_read_registry());
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path.clone(),
        agent,
        workspace_root,
        role_provider_builder,
    )?;

    worker.send(WorkerCommand::SubmitPlanPrompt {
        prompt: "plan README typo review".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker
        .recv_until(|message| matches!(message, WorkerMessage::PlanRunStarted { .. }))
        .context("explicit plan review did not start")?;
    let finished = worker
        .recv_until_with_timeout(Duration::from_secs(10), |message| {
            matches!(
                message,
                WorkerMessage::PlanRunFinished { .. } | WorkerMessage::RunFailed(_)
            )
        })
        .map_err(|error| {
            let entries = JsonlSessionStore::read_entries(&session_log_path).unwrap_or_default();
            let child_entries = entries
                .iter()
                .find_map(|entry| match entry {
                    SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt)) => {
                        Some(attempt.child_session_ref.resolve(
                            session_log_path.parent().unwrap_or_else(|| std::path::Path::new(".")),
                        ))
                    }
                    _ => None,
                })
                .and_then(|path| JsonlSessionStore::read_entries(path).ok())
                .unwrap_or_default();
            anyhow!(
                "explicit plan review did not finish: {error:#}; durable entries: {entries:?}; child entries: {child_entries:?}"
            )
        })?;
    if let WorkerMessage::RunFailed(error) = &finished {
        return Err(anyhow!("explicit plan review failed: {error}"));
    }
    let WorkerMessage::PlanRunFinished { entries, .. } = finished else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    let projection = PlanArtifactProjection::from_entries(&entries);
    let draft = projection
        .latest_pending_plan()
        .expect("plan run should append durable draft")
        .clone();

    worker.send(WorkerCommand::CreateTaskFromPlan {
        plan_id: draft.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash.clone(),
        start_mode: PlanTaskStartMode::CreateAndRun,
        permission_grant: None,
    })?;
    let created = worker
        .recv_until(|message| matches!(message, WorkerMessage::TaskCreatedFromPlan { .. }))
        .context("approved plan did not create its task")?;
    let WorkerMessage::TaskCreatedFromPlan {
        entry: created_task,
        start_mode,
        entries,
    } = created
    else {
        unreachable!("recv_until only returns TaskCreatedFromPlan");
    };
    assert_eq!(start_mode, PlanTaskStartMode::CreateAndRun);
    assert_eq!(created_task.plan_id, draft.plan_id);
    assert_eq!(created_task.plan_hash, draft.plan_hash);
    assert_eq!(created_task.task_plan_version, 0);
    assert!(created_task.step_mapping.is_empty());
    assert!(created_task.stale_reason.is_none());
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskMaterializationPreparedV1(_))
    )));
    let projection = sigil_kernel::TaskStateProjection::from_entries(&entries);
    let adopted_task = projection
        .tasks
        .get(&created_task.task_id)
        .expect("approved plan should have direct execution authority");
    assert!(adopted_task.plans.is_empty());
    assert!(adopted_task.latest_plan_version.is_none());
    assert!(adopted_task.direct_execution_admission.is_some());
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(_))
    )));
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(_))
    )));

    let started = worker
        .recv_until(|message| matches!(message, WorkerMessage::TaskRunStarted { .. }))
        .context("approved task did not start")?;
    assert!(matches!(
        started,
        WorkerMessage::TaskRunStarted { ref objective, .. }
            if objective.contains("Execute the following user-approved Plan")
                && objective.contains("Inspect README.md")
    ));

    let finished = worker
        .recv_until_with_timeout(Duration::from_secs(10), |message| {
            matches!(
                message,
                WorkerMessage::TaskRunFinished { .. } | WorkerMessage::RunFailed(_)
            )
        })
        .map_err(|error| {
            let entries = sigil_kernel::JsonlSessionStore::read_entries(&session_log_path)
                .unwrap_or_default();
            anyhow!(
                "{error}; durable entries: {}",
                control_entry_debug(&entries)
            )
        })?;
    if let WorkerMessage::RunFailed(error) = &finished {
        return Err(anyhow!("task run failed: {error}"));
    }
    let WorkerMessage::TaskRunFinished {
        task_id,
        status,
        entries,
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };
    assert_eq!(task_id, created_task.task_id.as_str());
    assert_eq!(status, TaskRunStatus::Completed);

    let task_projection = sigil_kernel::TaskStateProjection::from_entries(&entries);
    let task = task_projection
        .tasks
        .get(&created_task.task_id)
        .expect("approved plan should retain direct execution authority");
    assert!(task.plans.is_empty());
    assert!(task.latest_plan_version.is_none());
    assert!(task.direct_execution_admission.is_some());
    assert!(task.direct_execution_attempts.values().any(|attempt| {
        attempt.status == sigil_kernel::TaskParticipantAttemptStatus::Completed
    }));
    assert!(
        !entries
            .iter()
            .any(|entry| matches!(entry, SessionLogEntry::Control(ControlEntry::TaskStep(_))))
    );
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskParticipantAttempt(attempt))
            if attempt.purpose == sigil_kernel::TaskParticipantPurpose::Planner
    )));
    let plan_artifacts = sigil_kernel::PlanArtifactProjection::from_entries(&entries);
    assert!(
        plan_artifacts
            .materialization_for_task(&created_task.task_id)
            .is_none()
    );
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(_))
    )));
    assert!(
        worker
            .recv_until_with_timeout(Duration::from_millis(100), |message| {
                matches!(message, WorkerMessage::RunFailed(_))
            })
            .is_err(),
        "a naturally completed task must not emit a trailing RunFailed"
    );

    worker.shutdown()?;
    Ok(())
}

#[test]
fn approved_plan_direct_execution_can_pause_and_resume_without_a_task_plan() -> Result<()> {
    let timeout = Duration::from_secs(20);
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-plan-direct-pause-resume.jsonl");
    let root_config = routed_unauthenticated_test_root_config(&workspace_root, "planned-model");
    let draft_args = r##"{
  "schema_version": 1,
  "outcome": "draft",
  "content": "# Pause and resume direct execution\n\n1. Inspect the approved objective"
}"##;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(
        submit_plan_review_result_chunks("direct-pause-plan", draft_args),
    )]);
    let (role_provider_builder, role_stream_started_rx) =
        planned_role_provider_builder_with_stream_start_signal(vec![
            StreamPlan::Pending,
            StreamPlan::Chunks(vec![
                ProviderChunk::TextDelta("resumed direct execution completed".to_owned()),
                ProviderChunk::Done,
            ]),
        ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, task_workspace_read_registry()),
        workspace_root,
        role_provider_builder,
    )?;

    worker.send(WorkerCommand::SubmitPlanPrompt {
        prompt: "plan a pausable direct execution".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker
        .recv_until_with_timeout(timeout, |message| {
            matches!(message, WorkerMessage::PlanRunFinished { .. })
        })
        .context("waiting for direct pause plan review")?;
    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    let draft = PlanArtifactProjection::from_entries(&entries)
        .latest_pending_plan()
        .context("direct pause test plan should be reviewable")?
        .clone();

    worker.send(WorkerCommand::CreateTaskFromPlan {
        plan_id: draft.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash,
        start_mode: PlanTaskStartMode::CreateAndRun,
        permission_grant: None,
    })?;
    let created = worker
        .recv_until_with_timeout(timeout, |message| {
            matches!(message, WorkerMessage::TaskCreatedFromPlan { .. })
        })
        .context("waiting for direct pause Task adoption")?;
    let WorkerMessage::TaskCreatedFromPlan { entry, entries, .. } = created else {
        unreachable!("recv_until only returns TaskCreatedFromPlan");
    };
    let task = sigil_kernel::TaskStateProjection::from_entries(&entries)
        .tasks
        .get(&entry.task_id)
        .cloned()
        .context("direct Task should project")?;
    let admission = task
        .direct_execution_admission
        .context("direct execution admission should be durable")?;
    assert!(task.plans.is_empty());
    let _ = worker
        .recv_until_with_timeout(timeout, |message| {
            matches!(message, WorkerMessage::TaskRunStarted { .. })
        })
        .context("waiting for direct pause Task start")?;
    role_stream_started_rx
        .recv_timeout(timeout)
        .context("waiting for direct executor stream")?;
    wait_for_session_entry(&session_log_path, |candidate| {
        matches!(
            candidate,
            SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAttemptV1(attempt))
                if attempt.task_id == entry.task_id
                    && attempt.status == sigil_kernel::TaskParticipantAttemptStatus::Started
        )
    })
    .context("waiting for direct execution attempt admission")?;

    worker.send(WorkerCommand::PauseTask {
        request: TaskPauseRequest::direct(entry.task_id.clone(), admission.admission_id),
    })?;
    let paused = worker
        .recv_until_with_timeout(timeout, |message| {
            matches!(message, WorkerMessage::TaskRunPaused { .. })
        })
        .context("waiting for direct Task pause")?;
    let WorkerMessage::TaskRunPaused { entries, .. } = paused else {
        unreachable!("recv_until only returns TaskRunPaused");
    };
    let paused_task = sigil_kernel::TaskStateProjection::from_entries(&entries)
        .tasks
        .get(&entry.task_id)
        .cloned()
        .context("paused direct Task should remain durable")?;
    assert_eq!(paused_task.status, TaskRunStatus::Paused);
    assert!(
        paused_task
            .direct_execution_attempts
            .values()
            .any(|attempt| {
                attempt.status == sigil_kernel::TaskParticipantAttemptStatus::Interrupted
            })
    );

    worker.send(WorkerCommand::ContinueTask {
        task_id: Some(entry.task_id.as_str().to_owned()),
        guidance: None,
    })?;
    let finished = worker
        .recv_until_with_timeout(timeout, |message| {
            matches!(message, WorkerMessage::TaskRunFinished { .. })
        })
        .context("waiting for resumed direct Task completion")?;
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };
    assert_eq!(status, TaskRunStatus::Completed);
    let completed_task = sigil_kernel::TaskStateProjection::from_entries(&entries)
        .tasks
        .get(&entry.task_id)
        .cloned()
        .context("completed direct Task should project")?;
    assert!(completed_task.plans.is_empty());
    assert!(
        completed_task
            .direct_execution_attempts
            .values()
            .any(|attempt| {
                attempt.status == sigil_kernel::TaskParticipantAttemptStatus::Completed
            })
    );

    worker.shutdown()?;
    Ok(())
}

fn control_entry_debug(entries: &[SessionLogEntry]) -> String {
    entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskRun(run)) => Some(format!(
                "TaskRun({:?},{})",
                run.status,
                run.reason.as_deref().unwrap_or("")
            )),
            SessionLogEntry::Control(ControlEntry::TaskPlan(plan)) => Some(format!(
                "TaskPlan({:?},steps={})",
                plan.status,
                plan.steps.len()
            )),
            SessionLogEntry::Control(ControlEntry::TaskStep(step)) => Some(format!(
                "TaskStep({},{:?})",
                step.step_id.as_str(),
                step.status
            )),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" -> ")
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
        "routing microturn records exactly one PlanReview decision"
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
