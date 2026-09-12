use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, mpsc},
};

use sigil_kernel::{
    AgentConfig, CodeIntelligenceConfig, CompactionConfig, ContextSource, ControlEntry,
    DurableEventType, JsonlSessionStore, McpServerConfig, MemoryConfig,
    MutationArtifactCleanupRequested, MutationArtifactLifecycleRecorded,
    MutationArtifactLifecycleStatus, MutationEventRecorder, PermissionConfig, PublicRunEventKind,
    RootConfig, RunEvent, Session, SessionConfig, SessionLogEntry, SessionStreamRecord,
    StorageConfig, TaskConfig, TaskId, ToolEffect, VerificationCheckConfig, VerificationConfig,
    WorkspaceConfig, WorkspaceTrust, WorkspaceTrustDecisionEntry, bytes_hash,
    config::TerminalConfig, stable_workspace_id,
};

use crate::runner::{
    event_bridge::ChannelEventHandler,
    protocol::WorkerMessage,
    worker_loop::{
        PlanReviewExecutionResult, VerificationCheckPromotionKind,
        VerificationCheckPromotionOutcome, append_mcp_elicitation_audits, append_plan_draft,
        chat_agent_run_input_with_repo_context, clean_mutation_artifacts,
        commit_tui_plan_review_revision_waiting, configured_max_parallel_changeset_steps,
        configured_max_parallel_read_steps, configured_provider_route_concurrency_limit,
        delete_mutation_artifact, deliver_durable_revision_terminal_after_audit,
        materialize_task_verification_config, prepare_task_run_cancellation,
        preserve_revision_result_after_audit, promote_workspace_verification_check,
        revision_terminal_worker_message, tui_plan_review_result_from_durable_revision_outcome,
    },
};

#[test]
fn worker_revision_waiting_commits_the_exact_public_outbox_before_tui_wakeup() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("revision-waiting.jsonl");
    let mut session = Session::load_from_store(
        "tui-worker-test",
        "test-model",
        JsonlSessionStore::new(&session_path)?,
    )?;
    let session_id = session.session_scope_id().to_owned();
    let source_turn = sigil_kernel::ConversationTurnRef::new(
        &session_id,
        "revision-waiting-source-message",
        "revision-waiting-root-run",
    )?;
    let review_id = sigil_kernel::PlanReviewId::new("revision-waiting-review")?;
    let base_attempt_id = sigil_kernel::PlanReviewAttemptId::new("revision-waiting-base")?;
    let base_plan_id = sigil_kernel::PlanId::new("revision-waiting-base-plan")?;
    let base = sigil_kernel::plain_text_plan_draft_entry_with_plan_id(
        base_plan_id.clone(),
        "Keep the durable public-event contract.",
        sigil_kernel::PlanSourceRef {
            source_turn: Some(source_turn.clone()),
            plan_review_id: Some(review_id.clone()),
            ..sigil_kernel::PlanSourceRef::default()
        },
        10,
        None,
    )?
    .expect("test fixture needs a base plan");
    let base_attempt = sigil_kernel::PlanReviewAttemptEntry {
        plan_review_id: review_id.clone(),
        attempt_id: base_attempt_id.clone(),
        plan_id: base_plan_id,
        source: sigil_kernel::PlanReviewSource::ExplicitPlanCommand,
        source_turn: source_turn.clone(),
        explicit_objective: Some("Keep the durable public-event contract.".to_owned()),
        route_decision_id: None,
        child_session_ref: sigil_kernel::plan_review_child_session_ref(
            &review_id,
            &base_attempt_id,
        ),
        finalizer_session_ref: Some(sigil_kernel::plan_review_finalizer_session_ref(
            &review_id,
            &base_attempt_id,
            1,
        )),
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        workspace_snapshot_id: None,
        pending_user_input: None,
        status: sigil_kernel::PlanReviewAttemptStatus::DraftReady,
        terminal_reason: None,
        recorded_at_ms: 11,
    };
    // Started and DraftReady retain the same immutable lifecycle binding.
    let mut base_started = base_attempt.clone();
    base_started.status = sigil_kernel::PlanReviewAttemptStatus::Started;
    base_started.recorded_at_ms = 9;
    session.append_control(ControlEntry::PlanReviewAttempt(base_started))?;
    session.append_controls(vec![
        ControlEntry::PlanDraftCreated(base.clone()),
        ControlEntry::PlanReviewAttempt(base_attempt),
    ])?;
    let guidance = sigil_runtime::PlanReviewCoordinator::request_plan_revision_guidance(
        &mut session,
        &base.plan_id,
        &base.plan_hash,
        12,
    )?;
    let (_, revision) = sigil_runtime::PlanReviewCoordinator::accept_plan_revision_guidance(
        &mut session,
        sigil_kernel::UserInputDecisionCommandV1 {
            identity: guidance.request.identity,
            request_hash: guidance.request_hash,
            command_id: sigil_kernel::UserInputCommandId::new("revision-waiting-guidance")?,
            decision: sigil_kernel::UserInputDecisionV1::Submitted {
                answers: vec![sigil_kernel::UserInputAnswerV1 {
                    question_id: "revision_guidance".to_owned(),
                    value: sigil_kernel::UserInputAnswerValueV1::Text {
                        value: "Ask one bounded research question first.".to_owned(),
                    },
                }],
            },
        },
        None,
        13,
    )?;
    let revision = revision.expect("accepted guidance must prepare a revision worker run");
    sigil_runtime::PlanReviewCoordinator::ensure_revision_attempt_started(
        &mut session,
        &revision,
        14,
    )?;
    let pending = sigil_kernel::PublicUserInputRequestV1 {
        identity: sigil_kernel::UserInputIdentityV1 {
            session_scope_id: sigil_kernel::SessionScopeId::new(&session_id)?,
            root_logical_run_id: sigil_kernel::LogicalRunId::new(revision.child_logical_run_id())?,
            source_thread_id: sigil_kernel::AgentThreadId::new("main")?,
            request_id: sigil_kernel::UserInputRequestId::new("revision-waiting-research")?,
            generation: 1,
            source_binding_hash: format!("sha256:{}", "a".repeat(64)),
        },
        request_hash: format!("sha256:{}", "b".repeat(64)),
        source: sigil_kernel::UserInputSourceV1::PlanReviewResearch {
            plan_review_id: revision.plan_review_id.clone(),
            attempt_id: revision.attempt_id.clone(),
        },
        purpose: sigil_kernel::UserInputPurposeV1::Clarification,
        prompt: "Which invariant is still unproven?".to_owned(),
        questions: Vec::new(),
        allowed_actions: vec![sigil_kernel::UserInputActionV1::Submit],
        requested_at_unix_ms: 15,
        status: sigil_kernel::UserInputStatusV1::Requested,
        answer_receipt: None,
        resolution: None,
    };

    let outbox = commit_tui_plan_review_revision_waiting(&mut session, &revision, &pending)
        .map_err(anyhow::Error::msg)?;
    assert!(matches!(
        &outbox.event.event,
        PublicRunEventKind::RunAwaitingUserInput { request_id, generation: 1, .. }
            if request_id == "revision-waiting-research"
    ));
    let records = JsonlSessionStore::read_event_records(&session_path)?;
    let outbox_projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    assert!(
        outbox_projection
            .pending_for_adapter("tui")
            .iter()
            .any(|entry| {
                entry.public_event_id == outbox.public_event_id
                    && entry.event.sequence == outbox.event.sequence
            })
    );
    let waiting_record = records
        .iter()
        .find(|record| record.stored_event().event_id == outbox.domain_event_id)
        .expect("waiting domain record must be committed with the public outbox");
    let public_record = records
        .iter()
        .find(|record| record.stored_event().event_id == outbox.public_event_id)
        .expect("waiting public record must be committed with the domain record");
    assert_eq!(
        public_record.stream_sequence(),
        waiting_record.stream_sequence().saturating_add(1),
        "the real worker Waiting transition must use the kernel's adjacent atomic pair"
    );
    Ok(())
}

#[test]
fn durable_revision_terminals_keep_their_typed_worker_projection() {
    let cancelled = tui_plan_review_result_from_durable_revision_outcome(
        sigil_runtime::PlanReviewRunOutcome::Cancelled,
    )
    .expect("cancelled revision is a durable terminal, not an adapter error");
    assert!(matches!(cancelled, PlanReviewExecutionResult::Cancelled));

    let interrupted = tui_plan_review_result_from_durable_revision_outcome(
        sigil_runtime::PlanReviewRunOutcome::Interrupted("provider stopped".to_owned()),
    )
    .expect("interrupted revision is a durable terminal, not an adapter error");
    assert!(matches!(
        interrupted,
        PlanReviewExecutionResult::Interrupted { ref reason }
            if reason == "provider stopped"
    ));

    let entries = Vec::new();
    let cancelled = revision_terminal_worker_message(
        &PublicRunEventKind::RunCancelled,
        "worker-owned-session".to_owned(),
        std::path::Path::new("session.jsonl"),
        "provider".to_owned(),
        "model".to_owned(),
        entries.clone(),
    )
    .expect("exact cancelled outbox must map to the existing typed message");
    assert!(matches!(cancelled, WorkerMessage::RunCancelled { .. }));

    let interrupted = revision_terminal_worker_message(
        &PublicRunEventKind::RunInterrupted {
            reason: "durable interruption".to_owned(),
        },
        "worker-owned-session".to_owned(),
        std::path::Path::new("session.jsonl"),
        "provider".to_owned(),
        "model".to_owned(),
        entries,
    )
    .expect("exact interrupted outbox must map to the existing typed message");
    assert!(matches!(
        interrupted,
        WorkerMessage::RunInterrupted { ref reason, .. } if reason == "durable interruption"
    ));

    let finished = revision_terminal_worker_message(
        &PublicRunEventKind::RunFinished {
            final_text: "the original durable revision result".to_owned(),
        },
        "worker-owned-session".to_owned(),
        std::path::Path::new("session.jsonl"),
        "provider".to_owned(),
        "model".to_owned(),
        Vec::new(),
    )
    .expect("a later adapter-side error must not replace the durable success projection");
    assert!(matches!(
        finished,
        WorkerMessage::PlanRunFinished { ref result, .. }
            if result.final_text == "the original durable revision result"
    ));
}

#[test]
fn durable_revision_success_survives_a_real_post_run_audit_append_failure() {
    let audit_buffer = Arc::new(Mutex::new(Vec::<ControlEntry>::new()));
    let poisoning_buffer = Arc::clone(&audit_buffer);
    let _ = std::panic::catch_unwind(move || {
        let _guard = poisoning_buffer
            .lock()
            .expect("new audit buffer should be lockable before fault injection");
        panic!("inject post-run audit lock failure");
    });

    let mut session = Session::new("provider", "model");
    let audit_result = append_mcp_elicitation_audits(&mut session, &audit_buffer);
    assert!(
        audit_result.is_err(),
        "the test must exercise the real post-run audit append failure"
    );

    let (message_tx, message_rx) = mpsc::channel();
    let payload = preserve_revision_result_after_audit(
        Ok(PlanReviewExecutionResult::Finished(
            sigil_kernel::AgentRunResult {
                final_text: "original durable revision result".to_owned(),
                tool_calls: 0,
                final_message_id: None,
                completion_claim: None,
            },
        )),
        audit_result,
        &message_tx,
    )
    .expect("audit delivery degradation must not replace the completed revision result");
    assert!(matches!(
        payload,
        PlanReviewExecutionResult::Finished(result)
            if result.final_text == "original durable revision result"
    ));
    assert!(matches!(
        message_rx
            .recv()
            .expect("audit degradation should be surfaced as a Notice"),
        WorkerMessage::Notice(message)
            if message.contains("revision audit delivery failed; durable execution state is unchanged")
    ));
}

#[test]
fn active_cancel_delivery_keeps_the_exact_revision_terminal_after_audit_failure() {
    let audit_buffer = Arc::new(Mutex::new(Vec::<ControlEntry>::new()));
    let poisoning_buffer = Arc::clone(&audit_buffer);
    let _ = std::panic::catch_unwind(move || {
        let _guard = poisoning_buffer
            .lock()
            .expect("new audit buffer should be lockable before fault injection");
        panic!("inject active-cancel audit lock failure");
    });
    let mut session = Session::new("provider", "model");
    let audit_result = append_mcp_elicitation_audits(&mut session, &audit_buffer);
    assert!(audit_result.is_err());

    let (message_tx, message_rx) = mpsc::channel();
    deliver_durable_revision_terminal_after_audit(
        &PublicRunEventKind::RunCancelled,
        audit_result,
        std::path::Path::new("session.jsonl"),
        &session,
        &message_tx,
    );
    assert!(matches!(
        message_rx
            .recv()
            .expect("audit degradation should be surfaced before terminal delivery"),
        WorkerMessage::Notice(message)
            if message.contains("revision audit delivery failed; durable execution state is unchanged")
    ));
    assert!(matches!(
        message_rx
            .recv()
            .expect("exact durable terminal must still reach the active-cancel consumer"),
        WorkerMessage::RunCancelled { .. }
    ));
}

#[test]
fn append_plan_draft_preserves_plain_model_output_without_graph_contract() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root_config = root_config_with_checks(temp.path(), Vec::new());
    let session_log_path = temp.path().join("session-plan-plain.jsonl");
    let mut current_session = Some(Session::new("deepseek", "deepseek-v4-flash"));
    let text = "# Implementation plan\n\n1. Inspect the live path.\n2. Apply and verify.";

    let draft = append_plan_draft(
        &root_config,
        temp.path(),
        &session_log_path,
        &mut current_session,
        text,
        Some("message-1".to_owned()),
        7,
    )
    .expect("plain output should create a durable Plan")
    .expect("non-empty plain output should not be dropped");

    assert_eq!(draft.summary, "Implementation plan");
    assert_eq!(draft.inline_text.as_deref(), Some(text));
    assert!(draft.steps.is_empty());
    assert!(
        current_session
            .expect("session remains available")
            .entries()
            .iter()
            .any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::PlanDraftCreated(entry))
                    if entry.plan_id == draft.plan_id && entry.plan_hash == draft.plan_hash
            ))
    );
}

#[test]
fn task_parallel_concurrency_uses_config_and_clamps_zero() {
    let mut config = TaskConfig::default();
    assert_eq!(configured_max_parallel_read_steps(&config), 4);
    assert_eq!(configured_max_parallel_changeset_steps(&config), 2);
    assert_eq!(configured_provider_route_concurrency_limit(&config), 4);

    config.max_parallel_read_steps = 2;
    config.max_parallel_changeset_steps = 3;
    assert_eq!(configured_max_parallel_read_steps(&config), 2);
    assert_eq!(configured_max_parallel_changeset_steps(&config), 3);
    assert_eq!(configured_provider_route_concurrency_limit(&config), 3);

    config.max_parallel_read_steps = 0;
    config.max_parallel_changeset_steps = 0;
    assert_eq!(configured_max_parallel_read_steps(&config), 1);
    assert_eq!(configured_max_parallel_changeset_steps(&config), 1);
    assert_eq!(configured_provider_route_concurrency_limit(&config), 1);
}

#[test]
fn tui_task_cancellation_adapter_uses_durable_shared_binding() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl")).expect("session store");
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)
        .expect("initialize current worker session");
    let task_id = TaskId::new("task-1").expect("task id");

    let (_owner, _recorder, handle, task_guard) =
        prepare_task_run_cancellation(&mut session, &task_id).expect("task cancellation");

    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(binding))
            if binding.task_id == task_id && binding.run_scope_id == handle.scope_id()
    )));
    drop(task_guard);
}

#[tokio::test]
async fn chat_agent_run_input_with_repo_context_attaches_repository_candidates() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("README.md"),
        "Sigil is a Rust coding agent with Desktop and TUI experiences.",
    )
    .expect("write README");

    let resolver = sigil_runtime::RequestContextResolver::request_local(temp.path().to_path_buf());
    let input = chat_agent_run_input_with_repo_context(
        &resolver,
        "summarize README.md".to_owned(),
        false,
        Vec::new(),
    )
    .await;

    assert!(input.persisted_user_message.is_some());
    assert!(input.runtime_context.items.iter().any(|item| {
        item.id == "repo-file:README.md" && matches!(item.source, ContextSource::RepositoryFile)
    }));
    assert!(input.runtime_context.items.iter().any(|item| {
        item.id == "lsp-context:unavailable" && matches!(item.source, ContextSource::LspSymbol)
    }));
}

#[tokio::test]
async fn chat_agent_run_input_with_repo_context_preserves_plan_mode_transience() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("README.md"), "plan context").expect("write README");

    let resolver = sigil_runtime::RequestContextResolver::request_local(temp.path().to_path_buf());
    let input = chat_agent_run_input_with_repo_context(
        &resolver,
        "plan from README.md".to_owned(),
        true,
        Vec::new(),
    )
    .await;

    assert!(input.persisted_user_message.is_none());
    assert!(input.runtime_context.items.iter().any(|item| {
        item.id == "repo-file:README.md" && matches!(item.source, ContextSource::RepositoryFile)
    }));
}

#[test]
fn tui_materialize_task_verification_wrapper_records_specs_policy_and_events() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write Cargo.toml");
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut root_config = root_config_with_checks(
        temp.path(),
        vec![VerificationCheckConfig {
            id: "cargo-test".to_owned(),
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: None,
            effect: ToolEffect::ReadOnly,
        }],
    );
    root_config.verification.scope.profile = sigil_kernel::VerificationScopeProfile::Node;
    let (tx, rx) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(tx);
    let task_id = TaskId::new("task-1").expect("task id");

    materialize_task_verification_config(
        &mut session,
        &mut handler,
        &root_config,
        temp.path(),
        &task_id,
    )
    .expect("config materializes");

    let projection = session.verification_state_projection();
    let scope = sigil_kernel::EvidenceScope::Task("task-1".to_owned());
    assert!(
        projection
            .check_spec(&scope, "cargo-test")
            .is_some_and(|entry| entry.trusted_check.source
                == sigil_kernel::CheckDiscoverySource::UserExplicitConfig)
    );
    assert!(projection.latest_policy(&scope).is_some_and(|entry| {
        entry.policy.required_checks.len() == 1
            && entry.policy.workspace_trust_requirement
                == sigil_kernel::WorkspaceTrustRequirement::None
            && entry
                .policy
                .verification_scope
                .exclude
                .contains(&".next/**".to_owned())
    }));
    let controls = rx
        .try_iter()
        .filter_map(|message| match message {
            WorkerMessage::Event(event) => match *event {
                RunEvent::Control(control) => Some(control),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        controls.as_slice(),
        [
            ControlEntry::CheckSpecRecorded(_),
            ControlEntry::VerificationPolicyChanged(_)
        ]
    ));

    let (tx, _rx) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(tx);
    materialize_task_verification_config(
        &mut session,
        &mut handler,
        &root_config,
        temp.path(),
        &task_id,
    )
    .expect("idempotent config materializes");
    let control_count = session
        .entries()
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                sigil_kernel::SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(_))
                    | sigil_kernel::SessionLogEntry::Control(
                        ControlEntry::VerificationPolicyChanged(_)
                    )
            )
        })
        .count();
    assert_eq!(control_count, 2);
}

#[test]
fn materialize_task_verification_config_skips_inapplicable_user_cargo_check() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let root_config = root_config_with_checks(
        temp.path(),
        vec![VerificationCheckConfig {
            id: "kernel-verification".to_owned(),
            command: "cargo".to_owned(),
            args: vec![
                "test".to_owned(),
                "-p".to_owned(),
                "sigil-kernel".to_owned(),
                "verification".to_owned(),
            ],
            cwd: None,
            effect: ToolEffect::ReadOnly,
        }],
    );
    let (tx, rx) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(tx);
    let task_id = TaskId::new("task-1").expect("task id");

    materialize_task_verification_config(
        &mut session,
        &mut handler,
        &root_config,
        temp.path(),
        &task_id,
    )
    .expect("config materializes");

    let projection = session.verification_state_projection();
    let scope = sigil_kernel::EvidenceScope::Task("task-1".to_owned());
    assert!(
        projection
            .check_spec(&scope, "kernel-verification")
            .is_none()
    );
    assert!(projection.latest_policy(&scope).is_none());
    assert!(rx.try_iter().next().is_none());
}

#[test]
fn materialize_task_verification_config_does_not_promote_repo_checks_without_trust() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write Cargo.toml");
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let root_config = root_config_with_checks(temp.path(), Vec::new());
    let (tx, rx) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(tx);
    let task_id = TaskId::new("task-1").expect("task id");

    materialize_task_verification_config(
        &mut session,
        &mut handler,
        &root_config,
        temp.path(),
        &task_id,
    )
    .expect("repo discovery should not fail");

    let projection = session.verification_state_projection();
    let scope = sigil_kernel::EvidenceScope::Task("task-1".to_owned());
    assert!(projection.check_spec(&scope, "cargo-test").is_none());
    assert!(projection.latest_policy(&scope).is_none());
    assert!(rx.try_iter().next().is_none());
}

#[test]
fn materialize_task_verification_config_does_not_require_repo_checks_for_trusted_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write Cargo.toml");
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let workspace_id = stable_workspace_id(temp.path()).expect("workspace id");
    session
        .append_control(ControlEntry::WorkspaceTrustDecision(
            WorkspaceTrustDecisionEntry {
                workspace_id,
                workspace_trust_snapshot_id: "trust-1".to_owned(),
                trust: WorkspaceTrust::Trusted,
                decided_by_event_id: Some("event-trust".to_owned()),
                reason: Some("test trusted workspace".to_owned()),
            },
        ))
        .expect("append trust decision");
    let root_config = root_config_with_checks(temp.path(), Vec::new());
    let (tx, rx) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(tx);
    let task_id = TaskId::new("task-1").expect("task id");

    materialize_task_verification_config(
        &mut session,
        &mut handler,
        &root_config,
        temp.path(),
        &task_id,
    )
    .expect("trusted repo checks materialize");

    let projection = session.verification_state_projection();
    let scope = sigil_kernel::EvidenceScope::Task("task-1".to_owned());
    assert!(projection.check_spec(&scope, "cargo-test").is_none());
    assert!(projection.latest_policy(&scope).is_none());
    assert!(rx.try_iter().next().is_none());
}

#[test]
fn materialize_task_verification_config_uses_workspace_check_promotion() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write Cargo.toml");
    let root_config = root_config_with_checks(temp.path(), Vec::new());
    let mut current_session = Some(Session::new("deepseek", "deepseek-v4-flash"));

    let promoted = promote_workspace_verification_check(
        temp.path(),
        &root_config,
        &mut current_session,
        "cargo-test",
        VerificationCheckPromotionKind::Approve,
    )
    .expect("approve repo-local check");
    let entry = match promoted {
        VerificationCheckPromotionOutcome::Promoted { entry } => *entry,
        VerificationCheckPromotionOutcome::AlreadyPromoted { .. } => {
            panic!("first approval should append a check spec")
        }
    };
    assert!(matches!(
        entry.scope,
        sigil_kernel::EvidenceScope::Workspace(_)
    ));
    assert!(matches!(
        entry.trusted_check.promoted_by,
        sigil_kernel::CheckPromotion::UserApproved { .. }
    ));

    let mut session = current_session.expect("session");
    let (tx, rx) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(tx);
    let task_id = TaskId::new("task-1").expect("task id");

    materialize_task_verification_config(
        &mut session,
        &mut handler,
        &root_config,
        temp.path(),
        &task_id,
    )
    .expect("approved repo check materializes");

    let projection = session.verification_state_projection();
    let task_scope = sigil_kernel::EvidenceScope::Task("task-1".to_owned());
    let check = projection
        .check_spec(&task_scope, "cargo-test")
        .expect("approved workspace check should materialize into task");
    assert!(matches!(
        check.trusted_check.promoted_by,
        sigil_kernel::CheckPromotion::UserApproved { .. }
    ));
    assert!(
        projection
            .latest_policy(&task_scope)
            .is_some_and(|entry| entry.policy.workspace_trust_requirement
                == sigil_kernel::WorkspaceTrustRequirement::ApprovalOrSandbox)
    );
    let controls = rx
        .try_iter()
        .filter_map(|message| match message {
            WorkerMessage::Event(event) => match *event {
                RunEvent::Control(control) => Some(control),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        controls.as_slice(),
        [
            ControlEntry::CheckSpecRecorded(_),
            ControlEntry::VerificationPolicyChanged(_)
        ]
    ));
}

#[test]
fn promote_workspace_verification_check_supports_sandbox_and_idempotence() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write Cargo.toml");
    let root_config = root_config_with_checks(temp.path(), Vec::new());
    let mut current_session = Some(Session::new("deepseek", "deepseek-v4-flash"));

    let promoted = promote_workspace_verification_check(
        temp.path(),
        &root_config,
        &mut current_session,
        "cargo-test",
        VerificationCheckPromotionKind::Sandbox,
    )
    .expect("sandbox repo-local check");
    let VerificationCheckPromotionOutcome::Promoted { entry } = promoted else {
        panic!("sandbox promotion should append a check spec");
    };
    assert!(matches!(
        entry.trusted_check.promoted_by,
        sigil_kernel::CheckPromotion::Sandboxed { .. }
    ));

    let repeated = promote_workspace_verification_check(
        temp.path(),
        &root_config,
        &mut current_session,
        "cargo-test",
        VerificationCheckPromotionKind::Sandbox,
    )
    .expect("idempotent sandbox promotion");
    assert!(matches!(
        repeated,
        VerificationCheckPromotionOutcome::AlreadyPromoted { ref check_spec_id }
            if check_spec_id == "cargo-test"
    ));
}

#[test]
fn clean_mutation_artifacts_applies_retention_policy_and_records_lifecycle() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    std::fs::write(&target, "old")?;
    let session_path = temp.path().join("sessions/session.jsonl");
    let store = JsonlSessionStore::new(session_path.clone())?;
    let recorder = MutationEventRecorder::new(store.clone());
    let coordinator = recorder.coordinator(&workspace, "tool-call-cleanup", None)?;
    let new_content = b"new";
    let prepared =
        coordinator.prepare_file("note.txt", target.clone(), Some(bytes_hash(new_content)))?;
    coordinator.commit_write(&prepared, new_content)?;

    let mut root_config = root_config_with_checks(&workspace, Vec::new());
    root_config
        .storage
        .mutation_artifact_retention
        .max_artifacts = Some(0);
    root_config.storage.mutation_artifact_retention.max_bytes = None;
    root_config
        .storage
        .mutation_artifact_retention
        .expire_older_than_ms = None;
    let current_session = Some(Session::load_from_store(
        "deepseek",
        "deepseek-v4-flash",
        store.clone(),
    )?);

    let report = clean_mutation_artifacts(
        &root_config,
        &session_path,
        &current_session,
        &sigil_kernel::MutationArtifactCleanupTarget::Recommended,
    )
    .expect("cleanup should apply retention");

    assert_eq!(report.scanned_artifacts, 1);
    assert_eq!(report.expired_artifacts, 1);
    assert_eq!(report.unavailable_artifacts, 0);
    assert_eq!(report.lifecycle_events.len(), 1);
    let cleanup_requests = JsonlSessionStore::read_event_records(&session_path)?
        .into_iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type
                    == DurableEventType::MutationArtifactCleanupRequested.as_str() =>
            {
                Some(
                    serde_json::from_value::<MutationArtifactCleanupRequested>(event.payload)
                        .expect("cleanup request payload should decode"),
                )
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        cleanup_requests.as_slice(),
        [MutationArtifactCleanupRequested {
            target: sigil_kernel::MutationArtifactCleanupTarget::Recommended,
            candidate_artifacts: 1,
            ..
        }]
    ));
    let lifecycle_records = JsonlSessionStore::read_event_records(&session_path)?
        .into_iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type
                    == DurableEventType::MutationArtifactLifecycleRecorded.as_str() =>
            {
                Some(
                    serde_json::from_value::<MutationArtifactLifecycleRecorded>(event.payload)
                        .expect("lifecycle payload should decode"),
                )
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        lifecycle_records.as_slice(),
        [MutationArtifactLifecycleRecorded {
            status: MutationArtifactLifecycleStatus::Expired,
            ..
        }]
    ));
    assert_eq!(
        lifecycle_records[0].reason.as_str(),
        "retention quota limit"
    );
    Ok(())
}

#[test]
fn delete_mutation_artifact_records_user_requested_lifecycle() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    std::fs::write(&target, "old")?;
    let session_path = temp.path().join("sessions/session.jsonl");
    let store = JsonlSessionStore::new(session_path.clone())?;
    let recorder = MutationEventRecorder::new(store.clone());
    let coordinator = recorder.coordinator(&workspace, "tool-call-delete", None)?;
    let new_content = b"new";
    let prepared =
        coordinator.prepare_file("note.txt", target.clone(), Some(bytes_hash(new_content)))?;
    coordinator.commit_write(&prepared, new_content)?;
    let artifact_id = recorder
        .list_mutation_artifacts()?
        .into_iter()
        .next()
        .expect("artifact should exist")
        .artifact_id;
    let current_session = Some(Session::load_from_store(
        "deepseek",
        "deepseek-v4-flash",
        store.clone(),
    )?);

    let payload = delete_mutation_artifact(&session_path, &current_session, &artifact_id)
        .expect("artifact deletion should record lifecycle");

    assert_eq!(payload.artifact_id, artifact_id);
    assert_eq!(payload.status, MutationArtifactLifecycleStatus::Deleted);
    assert_eq!(payload.reason, "user requested artifact deletion");
    assert!(recorder.list_mutation_artifacts()?.is_empty());
    Ok(())
}

fn root_config_with_checks(
    workspace_root: &std::path::Path,
    checks: Vec<VerificationCheckConfig>,
) -> RootConfig {
    RootConfig {
        config_version: 2,
        composition: Default::default(),
        workspace: WorkspaceConfig {
            root: workspace_root.display().to_string(),
        },
        storage: StorageConfig::default(),
        session: SessionConfig::default(),
        agent: AgentConfig {
            runtime_provider: "deepseek".to_owned(),
            connection: None,
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig::with_enabled(false),
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: CodeIntelligenceConfig::default(),
        terminal: TerminalConfig::default(),
        execution: Default::default(),
        verification: VerificationConfig {
            auto_run: sigil_kernel::VerificationAutoRunPolicy::Manual,
            checks,
            ..VerificationConfig::default()
        },
        appearance: Default::default(),
        task: TaskConfig::default(),
        connections: BTreeMap::new(),
        web: Default::default(),
        mcp_servers: Vec::<McpServerConfig>::new(),
    }
}
