use super::super::writer::SessionWriterFault;
use super::*;

fn active_session(store: &JsonlSessionStore) -> Result<Session> {
    let session = Session::new("test", "test").with_store(store.clone());
    session
        .conversation_run_lifecycle_recorder()?
        .append_started(&crate::ConversationRunStartedEntryV1::new("run-1", 1)?)?;
    Ok(session)
}

fn task_control() -> Result<ControlEntry> {
    Ok(ControlEntry::TaskRun(crate::TaskRunEntry {
        task_id: crate::TaskId::new("task-public")?,
        parent_session_ref: crate::SessionRef::new_relative("parent.jsonl")?,
        objective: "verify source publication".to_owned(),
        title: Some("Source publication".to_owned()),
        status: crate::TaskRunStatus::Started,
        reason: None,
    }))
}

fn continuation_state(message_id: &str) -> crate::ProviderContinuationState {
    crate::ProviderContinuationState {
        provider_name: "test".to_owned(),
        state_kind: "cursor".to_owned(),
        message_id: Some(message_id.to_owned()),
        opaque_blob: serde_json::json!({"cursor": "private-provider-state"}),
    }
}

fn tool_publication_bundle() -> Result<(Vec<SessionLogEntry>, Vec<SessionPublicEventProjectionV1>)>
{
    let source = crate::ToolResult::ok(
        "tool-call-1",
        "read_file",
        "private tool body",
        crate::ToolResultMeta::default(),
    );
    let (recorded, display) = crate::ToolResultRecordedV3::capture(
        &source,
        None,
        crate::ToolArtifactSensitivity::Ordinary,
    )?;
    let public = crate::ToolResult::ok(
        "tool-call-1",
        "read_file",
        display.preview,
        crate::ToolResultMeta::default(),
    );
    Ok((
        vec![
            SessionLogEntry::ToolResultV3(recorded),
            SessionLogEntry::Control(ControlEntry::Note {
                kind: "private_tool_companion".to_owned(),
                data: serde_json::json!({"secret": "do not publish"}),
            }),
        ],
        vec![SessionPublicEventProjectionV1::tool_result(0, public)],
    ))
}

#[test]
fn mixed_session_publication_commits_assistant_tool_and_private_entries_in_one_outbox_intent()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let mut session = active_session(&store)?;
    let assistant =
        crate::ModelMessage::assistant(Some("safe assistant answer".to_owned()), Vec::new());
    let continuation = continuation_state(&assistant.id);
    let (assistant_domain, assistant_public) = session.append_session_entries_with_public_outbox(
        vec![
            SessionLogEntry::Assistant(assistant.clone()),
            SessionLogEntry::Control(ControlEntry::Note {
                kind: "private_assistant_companion".to_owned(),
                data: serde_json::json!({"secret": "never public"}),
            }),
            SessionLogEntry::Control(ControlEntry::ContinuationStateSaved(continuation.clone())),
        ],
        vec![SessionPublicEventProjectionV1::assistant_message(
            0, assistant,
        )],
        "run-1",
        1,
    )?;
    let (tool_entries, tool_publications) = tool_publication_bundle()?;
    let (tool_domain, tool_public) = session.append_session_entries_with_public_outbox(
        tool_entries,
        tool_publications,
        "run-1",
        2,
    )?;

    assert_eq!(assistant_domain.len(), 3);
    assert_eq!(tool_domain.len(), 2);
    assert_eq!(assistant_public.len(), 1);
    assert_eq!(tool_public.len(), 1);
    let public = [assistant_public, tool_public].concat();
    assert_eq!(
        public
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(public[0].domain_event_id, assistant_domain[0].event_id);
    assert_eq!(public[1].domain_event_id, tool_domain[0].event_id);
    let public_json = serde_json::to_string(&public)?;
    assert!(!public_json.contains("never public"));
    assert!(!public_json.contains("do not publish"));
    assert!(!public_json.contains("private-provider-state"));
    assert!(
        public_json.contains("private tool body"),
        "ordinary tool output is allowed only through its bounded public preview"
    );

    let before_replay = std::fs::read(&path)?;
    drop(session);
    let reopened = JsonlSessionStore::open_existing(&path)?;
    let projection =
        PublicEventOutboxProjectionV1::from_records(&reopened.read_event_records_writer()?)?;
    assert_eq!(projection.events_in_order().len(), 2);
    assert_eq!(projection.pending_for_adapter("application").len(), 2);
    assert_eq!(
        std::fs::read(&path)?,
        before_replay,
        "replay reads the original bytes"
    );
    let receipt = crate::PublicEventDeliveryReceiptV1 {
        schema_version: crate::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: public[0].public_event_id.clone(),
        adapter: "application".to_owned(),
        delivered_at_unix_ms: 1,
    };
    assert!(crate::PublicEventOutboxRecorder::new(reopened.clone()).append_delivery(&receipt)?);
    let after_ack = reopened.read_event_records_writer()?;
    let after_ack_projection = PublicEventOutboxProjectionV1::from_records(&after_ack)?;
    assert_eq!(after_ack_projection.events_in_order().len(), 2);
    assert_eq!(
        after_ack
            .iter()
            .filter(|record| record.stored_event().event_kind()
                == Some(crate::DurableEventType::ToolResultRecordedV3))
            .count(),
        1,
        "ACK/replay appends only its receipt and never reruns the tool"
    );
    Ok(())
}

#[test]
fn mixed_session_publication_rejects_forged_source_or_dto_before_writing_bytes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let mut session = active_session(&store)?;
    let before = std::fs::read(&path)?;
    let entry_count = session.entries().len();
    let (entries, mut publications) = tool_publication_bundle()?;
    publications[0] = SessionPublicEventProjectionV1::tool_result(
        1,
        crate::ToolResult::ok(
            "tool-call-1",
            "read_file",
            "forged public body",
            crate::ToolResultMeta::default(),
        ),
    );
    assert!(
        session
            .append_session_entries_with_public_outbox(entries, publications, "run-1", 1)
            .is_err()
    );
    assert_eq!(std::fs::read(&path)?, before);
    assert_eq!(session.entries().len(), entry_count);

    let (entries, _) = tool_publication_bundle()?;
    assert!(
        session
            .append_session_entries_with_public_outbox(
                entries,
                vec![SessionPublicEventProjectionV1::tool_result(
                    0,
                    crate::ToolResult::ok(
                        "tool-call-1",
                        "read_file",
                        "forged public body",
                        crate::ToolResultMeta::default(),
                    ),
                )],
                "run-1",
                1,
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&path)?, before);
    assert_eq!(session.entries().len(), entry_count);
    Ok(())
}

#[test]
fn mixed_session_publication_recovers_writer_fault_without_reexecuting_the_producer() -> Result<()>
{
    for fault in [
        SessionWriterFault::BeforeWrite,
        SessionWriterFault::PartialFirstRecord,
        SessionWriterFault::PartialSecondRecord,
        SessionWriterFault::BeforeSync,
    ] {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session.jsonl");
        let store = JsonlSessionStore::new(&path)?;
        let mut session = active_session(&store)?;
        let (entries, publications) = tool_publication_bundle()?;
        store.inject_writer_fault(fault)?;
        let (domain, public) =
            session.append_session_entries_with_public_outbox(entries, publications, "run-1", 1)?;
        assert_eq!(domain.len(), 2, "{fault:?}");
        assert_eq!(public.len(), 1, "{fault:?}");
        assert_eq!(domain[0].event_id, public[0].domain_event_id, "{fault:?}");
        let bytes = std::fs::read(&path)?;
        drop(session);
        drop(store);

        let reopened = JsonlSessionStore::open_existing(&path)?;
        let records = reopened.read_event_records_writer()?;
        let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
        assert_eq!(projection.events_in_order().len(), 1, "{fault:?}");
        assert_eq!(
            serde_json::to_value(projection.events_in_order()[0])?,
            serde_json::to_value(&public[0])?,
            "{fault:?}"
        );
        assert_eq!(
            std::fs::read(&path)?,
            bytes,
            "reopen/replay must not run the tool producer again ({fault:?})"
        );
    }
    Ok(())
}

#[test]
fn mixed_session_publication_rejects_duplicate_projection_for_one_source_before_append()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let mut session = active_session(&store)?;
    let (entries, mut publications) = tool_publication_bundle()?;
    publications.push(publications[0].clone());
    let before = std::fs::read(&path)?;
    let entry_count = session.entries().len();

    let error = session
        .append_session_entries_with_public_outbox(entries, publications, "run-1", 1)
        .expect_err("one durable source cannot mint two public DTOs");

    assert!(
        error
            .to_string()
            .contains("exactly one projection per source")
    );
    assert_eq!(std::fs::read(&path)?, before);
    assert_eq!(session.entries().len(), entry_count);
    Ok(())
}

#[test]
fn control_publication_does_not_treat_an_ordinary_review_as_a_revision_run() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let mut session = Session::load_from_store("test", "test", store)?;
    let review_id = crate::PlanReviewId::new("ordinary-review")?;
    let attempt_id = crate::PlanReviewAttemptId::new("ordinary-attempt")?;
    let attempt = crate::PlanReviewAttemptEntry {
        plan_review_id: review_id.clone(),
        attempt_id: attempt_id.clone(),
        plan_id: crate::PlanId::new("ordinary-plan")?,
        source: crate::PlanReviewSource::ExplicitPlanCommand,
        source_turn: crate::ConversationTurnRef::new(
            session.session_scope_id(),
            "source-turn",
            "ordinary-root",
        )?,
        explicit_objective: Some("Prepare a plan".to_owned()),
        route_decision_id: None,
        child_session_ref: crate::plan_review_child_session_ref(&review_id, &attempt_id),
        finalizer_session_ref: Some(crate::plan_review_finalizer_session_ref(
            &review_id,
            &attempt_id,
            1,
        )),
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        workspace_snapshot_id: None,
        pending_user_input: None,
        status: crate::PlanReviewAttemptStatus::Started,
        terminal_reason: None,
        recorded_at_ms: 1,
    };
    let unrelated_run_id =
        super::super::plan_review_terminal::plan_review_revision_run_id(&attempt);
    session.append_control(ControlEntry::PlanReviewAttempt(attempt))?;
    let bytes = std::fs::read(&path)?;
    let entry_count = session.entries().len();
    let error = session
        .append_controls_with_public_outbox(vec![task_control()?], &unrelated_run_id, 1)
        .expect_err("an ordinary review does not own a separate revision run");
    assert!(error.to_string().contains("active durable run"));
    assert_eq!(session.entries().len(), entry_count);
    assert_eq!(std::fs::read(&path)?, bytes);
    Ok(())
}

#[test]
fn control_publication_rejects_missing_integration_context_before_domain_append() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let mut session = active_session(&store)?;
    let before = std::fs::read(&path)?;
    let lane = ControlEntry::IntegrationLaneChanged(crate::IntegrationLaneChanged {
        plan_id: crate::IntegrationPlanId::new("missing-plan")?,
        lane_id: crate::IntegrationLaneId::new("lane-1")?,
        status: crate::IntegrationLaneStatus::Ready,
        candidate: Some(crate::IntegrationLaneCandidate::ManagedRef {
            private_ref: "refs/sigil/integration/lane-1".to_owned(),
            base_commit: "b".repeat(40),
            candidate_commit: "a".repeat(40),
            workspace_snapshot_id: "snapshot-1".to_owned(),
        }),
        verification_check_ids: Vec::new(),
        reason: None,
    });
    let result = session.append_controls_with_public_outbox(vec![lane.clone()], "run-1", 1);
    assert!(
        result
            .expect_err("missing projection context must fail before append")
            .to_string()
            .contains("durable plan context")
    );
    assert_eq!(std::fs::read(&path)?, before);
    assert!(session.entries().is_empty());

    // A pre-existing domain-only lane did not promise a public DTO. Replaying a later linked
    // source must not retroactively apply the new explicit-publication contract to that lane.
    session.append_control(lane)?;
    session.append_controls_with_public_outbox(vec![task_control()?], "run-1", 1)?;
    let projection =
        PublicEventOutboxProjectionV1::from_records(&store.read_event_records_writer()?)?;
    assert_eq!(projection.events_in_order().len(), 1);
    Ok(())
}

#[test]
fn control_publication_recovers_exact_bundle_after_each_writer_fault() -> Result<()> {
    for fault in [
        SessionWriterFault::BeforeWrite,
        SessionWriterFault::PartialFirstRecord,
        SessionWriterFault::PartialSecondRecord,
        SessionWriterFault::BeforeSync,
    ] {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session.jsonl");
        let store = JsonlSessionStore::new(&path)?;
        let mut session = active_session(&store)?;
        store.inject_writer_fault(fault)?;
        let (domain, public) =
            session.append_controls_with_public_outbox(vec![task_control()?], "run-1", 1)?;
        assert_eq!(domain.len(), 1, "{fault:?}");
        assert_eq!(public.len(), 1);
        assert_eq!(domain[0].event_id, public[0].domain_event_id);
        assert_eq!(session.entries().len(), 1);
        let bytes = std::fs::read(&path)?;
        assert!(
            session
                .append_controls_with_public_outbox(vec![task_control()?], "run-1", 1)
                .is_err()
        );
        assert_eq!(session.entries().len(), 1);
        drop(session);
        drop(store);
        let reopened = JsonlSessionStore::open_existing(&path)?;
        let records = reopened.read_event_records_writer()?;
        assert_eq!(records.len(), 3);
        let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
        assert_eq!(
            serde_json::to_value(projection.events_in_order()[0])?,
            serde_json::to_value(&public[0])?
        );
        assert_eq!(std::fs::read(&path)?, bytes);
    }
    Ok(())
}

#[test]
fn control_publication_keeps_private_controls_out_of_the_public_stream() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = active_session(&store)?;
    let (private_domain, private_public) = session.append_controls_with_public_outbox(
        vec![ControlEntry::Note {
            kind: "private_audit".to_owned(),
            data: serde_json::json!({"message": "private audit only"}),
        }],
        "run-1",
        1,
    )?;
    assert_eq!(private_domain.len(), 1);
    assert!(private_public.is_empty());
    let (_, public) =
        session.append_controls_with_public_outbox(vec![task_control()?], "run-1", 1)?;
    let scans = store.writer_full_scan_count()?;
    for sequence in 2..10 {
        session.append_controls_with_public_outbox(vec![task_control()?], "run-1", sequence)?;
    }
    assert_eq!(store.writer_full_scan_count()?, scans);
    let records = store.read_event_records_writer()?;
    let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
    assert_eq!(projection.events_in_order().len(), 9);
    assert!(!serde_json::to_string(&public)?.contains("private audit only"));
    assert!(
        PublicEventOutboxRecorder::new(store)
            .append_outbox(&public[0])
            .is_err()
    );
    Ok(())
}

#[test]
fn control_publication_rebuilds_context_after_adopting_a_reordered_durable_prefix() -> Result<()> {
    for revision_recovery in [false, true] {
        let temp = tempfile::tempdir()?;
        let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
        let mut session = Session::load_from_store("test", "test", store.clone())?;
        session
            .conversation_run_lifecycle_recorder()?
            .append_started(&crate::ConversationRunStartedEntryV1::new("run-1", 1)?)?;
        session.append_controls_with_public_outbox(vec![task_control()?], "run-1", 1)?;

        let plan_id = crate::IntegrationPlanId::new("adopted-plan")?;
        let lane_id = crate::IntegrationLaneId::new("adopted-lane")?;
        let plan = ControlEntry::IntegrationPlanRecorded(crate::IntegrationPlanRecorded {
            plan: crate::IntegrationPlan {
                plan_id: plan_id.clone(),
                task_id: crate::TaskId::new("task-public")?,
                plan_version: 1,
                base_snapshot_id: "snapshot-1".to_owned(),
                base_representation: crate::IntegrationBaseRepresentation::Unknown,
                proposals: Vec::new(),
                conflicts: Vec::new(),
                lanes: vec![crate::IntegrationLaneSpec {
                    lane_id: lane_id.clone(),
                    proposals: Vec::new(),
                    verification_scope_hashes: Vec::new(),
                }],
            },
        });
        // A detached owner commits context before the next live-session append. Adopting the
        // canonical prefix inserts that context before the old cache cursor, not after it.
        store.append(&SessionLogEntry::Control(plan.clone()))?;
        session.append_controls_with_public_outbox(vec![task_control()?], "run-1", 2)?;
        if revision_recovery {
            session.adopt_plan_review_recovery_records(&store.read_event_records_writer()?)?;
        } else {
            session.record_durably_appended_controls([plan]);
        }
        let (_, publications) = session.append_controls_with_public_outbox(
            vec![ControlEntry::IntegrationLaneChanged(
                crate::IntegrationLaneChanged {
                    plan_id,
                    lane_id,
                    status: crate::IntegrationLaneStatus::Ready,
                    candidate: Some(crate::IntegrationLaneCandidate::ManagedRef {
                        private_ref: "refs/sigil/integration/adopted-lane".to_owned(),
                        base_commit: "b".repeat(40),
                        candidate_commit: "a".repeat(40),
                        workspace_snapshot_id: "snapshot-1".to_owned(),
                    }),
                    verification_check_ids: Vec::new(),
                    reason: None,
                },
            )],
            "run-1",
            3,
        )?;
        assert!(matches!(
            publications.as_slice(),
            [PublicEventOutboxEntryV1 {
                event: PublicRunEvent {
                    event: PublicRunEventKind::IntegrationLaneChanged { .. },
                    ..
                },
                ..
            }]
        ));
        PublicEventOutboxProjectionV1::from_records(&store.read_event_records_writer()?)?;
    }
    Ok(())
}

#[test]
fn control_publication_preserves_the_tool_execution_consumer_contract() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = active_session(&store)?;
    let execution = crate::ToolExecutionEntry {
        call_id: "call-1".to_owned(),
        tool_name: "read_file".to_owned(),
        status: crate::ToolExecutionStatus::Started,
        duration_ms: None,
        subjects: Vec::new(),
        changed_files: Vec::new(),
        metadata: crate::ToolResultMeta::default(),
        error: None,
        model_content_hash: None,
    };
    let (_, public) = session.append_controls_with_public_outbox(
        vec![ControlEntry::ToolExecution(Box::new(execution))],
        "run-1",
        1,
    )?;
    let PublicRunEventKind::Control { control } = &public[0].event.event else {
        bail!("tool execution consumer requires its public control");
    };
    assert_eq!(control.kind, "tool_execution");
    assert!(
        matches!(serde_json::from_value::<ControlEntry>(control.payload.clone().context("execution payload")?)?, ControlEntry::ToolExecution(execution) if execution.call_id == "call-1" && execution.status == crate::ToolExecutionStatus::Started)
    );
    PublicEventOutboxProjectionV1::from_records(&store.read_event_records_writer()?)?;
    Ok(())
}

#[test]
fn control_publication_replay_rejects_changed_dto_and_missing_source() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = active_session(&store)?;
    let (_, mut public) =
        session.append_controls_with_public_outbox(vec![task_control()?], "run-1", 1)?;
    let records = store.read_event_records_writer()?;
    let without_source = vec![records[0].clone(), records[2].clone()];
    assert!(PublicEventOutboxProjectionV1::from_records(&without_source).is_err());
    public[0].event.event = PublicRunEventKind::Notice {
        message: "not the source projection".to_owned(),
    };
    public[0].payload_digest = stable_event_hash(serde_json::to_vec(&public[0].event)?);
    let mut changed = records.clone();
    let mut envelope = changed[2].stored_event().clone();
    envelope.payload = serde_json::to_value(&public[0])?;
    envelope.record_checksum = envelope.compute_record_checksum()?;
    changed[2] = SessionStreamRecord::Stored(envelope);
    assert!(PublicEventOutboxProjectionV1::from_records(&changed).is_err());
    Ok(())
}

#[test]
fn control_publication_groups_multiple_dtos_under_one_domain_envelope() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = active_session(&store)?;
    let control = ControlEntry::TaskPlan(crate::TaskPlanEntry {
        task_id: crate::TaskId::new("task-public")?,
        plan_version: 1,
        status: crate::TaskPlanStatus::Accepted,
        steps: vec![crate::TaskStepSpec {
            step_id: crate::TaskStepId::new("step-public")?,
            title: "Inspect".to_owned(),
            display_name: None,
            detail: Some("private planner instructions".to_owned()),
            role: crate::AgentRole::SubagentRead,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: Some(crate::TaskStepMode::Read),
            isolation: Some(crate::TaskIsolationMode::SharedReadOnly),
        }],
        reason: None,
    });
    let (domain, public) = session.append_controls_with_public_outbox(vec![control], "run-1", 1)?;
    assert_eq!(domain.len(), 1);
    assert_eq!(public.len(), 2);
    assert_eq!(public[0].sequence, 1);
    assert_eq!(public[1].sequence, 2);
    assert!(
        public
            .iter()
            .all(|event| event.domain_event_id == domain[0].event_id)
    );
    assert!(!serde_json::to_string(&public)?.contains("private planner instructions"));
    let records = store.read_event_records_writer()?;
    PublicEventOutboxProjectionV1::from_records(&records)?;
    assert!(PublicEventOutboxProjectionV1::from_records(&records[..3]).is_err());
    Ok(())
}

#[test]
fn control_publication_requires_a_started_unfinalized_run_even_for_private_controls() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let mut session = Session::new("test", "test").with_store(store.clone());
    store.read_event_records_writer()?;
    let before = std::fs::read(&path)?;
    assert!(
        session
            .append_controls_with_public_outbox(vec![task_control()?], "run-1", 1)
            .is_err()
    );
    assert_eq!(std::fs::read(&path)?, before);
    let lifecycle = session.conversation_run_lifecycle_recorder()?;
    lifecycle.append_started(&crate::ConversationRunStartedEntryV1::new("run-1", 1)?)?;
    let terminal = crate::ConversationRunFinalizedEntryV1::new(
        "run-1",
        crate::ConversationRunTerminalStatusV1::Cancelled,
        None,
        None,
        2,
        &crate::SecretRedactor::empty(),
    )?;
    let public = PublicRunEvent::new(
        session.session_scope_id(),
        "run-1",
        1,
        PublicRunEventKind::RunCancelled,
    );
    lifecycle.append_finalized_with_outbox(
        &terminal,
        &PublicEventOutboxEntryV1 {
            schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            domain_event_id: "terminal-domain".to_owned(),
            public_event_id: "terminal-public".to_owned(),
            run_id: "run-1".to_owned(),
            sequence: 1,
            payload_digest: stable_event_hash(serde_json::to_vec(&public)?),
            event: public,
        },
    )?;
    let finalized = std::fs::read(&path)?;
    for control in [
        task_control()?,
        ControlEntry::Note {
            kind: "private".to_owned(),
            data: serde_json::Value::Null,
        },
    ] {
        assert!(
            session
                .append_controls_with_public_outbox(vec![control], "run-1", 2)
                .is_err()
        );
        assert_eq!(std::fs::read(&path)?, finalized);
    }
    assert!(session.entries().is_empty());
    Ok(())
}
