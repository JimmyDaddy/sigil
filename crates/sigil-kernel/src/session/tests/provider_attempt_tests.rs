use anyhow::Result;

use super::*;

fn cache_layout_proof() -> crate::CacheLayoutProofV1 {
    crate::CacheLayoutProofV1::from_request(
        &crate::CompletionRequest {
            provider_name: "test-provider".to_owned(),
            model_name: "test-model".to_owned(),
            messages: vec![crate::ModelMessage::user("current request")],
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(128),
            reasoning_effort: None,
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: None,
            background: false,
            store: false,
            deterministic_materialization: true,
            hosted_tools: Vec::new(),
        },
        None,
    )
    .expect("cache layout proof")
}

fn started() -> ProviderPhysicalAttemptStartedEntry {
    ProviderPhysicalAttemptStartedEntry {
        schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
        physical_attempt_id: "attempt-1".to_owned(),
        logical_run_id: "agent-run-1".to_owned(),
        purpose: ProviderPhysicalAttemptPurpose::ConversationGeneration,
        request_material_fingerprint: "hmac-sha256:started".to_owned(),
        provider_name: "test-provider".to_owned(),
        model_name: "test-model".to_owned(),
        request_envelope: None,
        cache_layout_proof: Some(cache_layout_proof()),
        semantic_cache_layout_proof_v2: None,
        started_at_unix_ms: 1,
    }
}

fn terminal(outcome: ProviderPhysicalAttemptOutcome) -> ProviderPhysicalAttemptTerminalEntry {
    ProviderPhysicalAttemptTerminalEntry {
        schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
        physical_attempt_id: "attempt-1".to_owned(),
        request_material_fingerprint: "hmac-sha256:started".to_owned(),
        outcome,
        rejection: None,
        provider_request_id: None,
        provider_response_id: None,
        durable_output_event_ids: Vec::new(),
        durable_side_effect_event_ids: Vec::new(),
        finished_at_unix_ms: 2,
    }
}

fn direct_event(
    event_type: DurableEventType,
    event_id: &str,
    sequence: u64,
    payload: serde_json::Value,
    correlation_id: Option<&str>,
    causation_id: Option<&str>,
) -> StoredEvent {
    let mut event = StoredEvent::new(
        event_type,
        event_type
            .expected_event_class()
            .expect("known physical-attempt event has a class"),
        event_id.to_owned(),
        "session-provider-attempt".to_owned(),
        sequence,
        payload,
    )
    .expect("physical-attempt event should build");
    event.correlation_id = correlation_id.map(str::to_owned);
    event.causation_id = causation_id.map(str::to_owned);
    event.record_checksum = event
        .compute_record_checksum()
        .expect("physical-attempt checksum should compute");
    event
}

#[test]
fn physical_attempt_projection_accepts_one_started_and_terminal() -> Result<()> {
    let start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        "event-start",
        1,
        serde_json::to_value(started())?,
        Some("event-start"),
        None,
    );
    let terminal = direct_event(
        DurableEventType::ProviderPhysicalAttemptTerminal,
        "event-terminal",
        2,
        serde_json::to_value(terminal(ProviderPhysicalAttemptOutcome::Completed))?,
        Some("event-start"),
        Some("event-start"),
    );

    let projection = ProviderPhysicalAttemptProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(terminal),
    ])?;
    let attempt = projection
        .attempt("attempt-1")
        .expect("physical attempt should project");
    assert!(attempt.terminal.is_some());
    assert!(projection.unfinished_attempts().is_empty());
    Ok(())
}

#[test]
fn physical_attempt_projection_rejects_cross_session_request_frontier() -> Result<()> {
    let request = crate::CompletionRequest {
        provider_name: "test-provider".to_owned(),
        model_name: "test-model".to_owned(),
        messages: vec![crate::ModelMessage::user("current request")],
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(128),
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    };
    let frozen = crate::FrozenProviderRequestMaterial::freeze("session-provider-attempt", request)?;
    let layout = frozen.cache_layout_proof(None)?;
    let mut entry = started();
    entry.request_material_fingerprint = frozen.fingerprint().to_owned();
    entry.cache_layout_proof = Some(layout.clone());
    entry.request_envelope = Some(frozen.request_envelope(
        &layout,
        Some(crate::ProviderRequestSourceFrontierV1 {
            session_id: "different-session".to_owned(),
            durable_end_offset: 1,
            stream_sequence: None,
            event_id: None,
            record_checksum: None,
        }),
        "context-epoch:root",
    )?);
    let start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        "event-start",
        1,
        serde_json::to_value(entry)?,
        Some("event-start"),
        None,
    );

    let error =
        ProviderPhysicalAttemptProjection::from_records(&[SessionStreamRecord::Stored(start)])
            .expect_err("cross-session source frontier must fail closed");
    assert!(error.to_string().contains("different session"));
    Ok(())
}

#[test]
fn physical_attempt_projection_resolves_confirmed_connect_retry_chain() -> Result<()> {
    let first_started = started();
    let first_start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        "event-start-1",
        1,
        serde_json::to_value(&first_started)?,
        Some("event-start-1"),
        None,
    );
    let mut first_terminal = terminal(ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption);
    first_terminal.rejection = Some(crate::ProviderRequestRejection::ConnectFailedBeforeDispatch);
    let first_terminal = direct_event(
        DurableEventType::ProviderPhysicalAttemptTerminal,
        "event-terminal-1",
        2,
        serde_json::to_value(first_terminal)?,
        Some("event-start-1"),
        Some("event-start-1"),
    );

    let mut second_started = started();
    second_started.physical_attempt_id = "attempt-2".to_owned();
    second_started.started_at_unix_ms = 3;
    let second_start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        "event-start-2",
        3,
        serde_json::to_value(&second_started)?,
        Some("event-start-2"),
        None,
    );
    let mut second_terminal = terminal(ProviderPhysicalAttemptOutcome::Completed);
    second_terminal.physical_attempt_id = second_started.physical_attempt_id.clone();
    second_terminal.finished_at_unix_ms = 4;
    let second_terminal = direct_event(
        DurableEventType::ProviderPhysicalAttemptTerminal,
        "event-terminal-2",
        4,
        serde_json::to_value(second_terminal)?,
        Some("event-start-2"),
        Some("event-start-2"),
    );

    let projection = ProviderPhysicalAttemptProjection::from_records(&[
        SessionStreamRecord::Stored(first_start),
        SessionStreamRecord::Stored(first_terminal),
        SessionStreamRecord::Stored(second_start),
        SessionStreamRecord::Stored(second_terminal),
    ])?;
    let attempts = projection.attempts_for_logical_run_id(first_started.logical_run_id.as_str());
    assert_eq!(
        attempts
            .iter()
            .map(|attempt| attempt.entry.physical_attempt_id.as_str())
            .collect::<Vec<_>>(),
        vec!["attempt-1", "attempt-2"]
    );
    assert_eq!(
        projection
            .effective_attempt_for_logical_run_id(first_started.logical_run_id.as_str())?
            .map(|attempt| attempt.entry.physical_attempt_id.as_str()),
        Some("attempt-2")
    );
    Ok(())
}

#[test]
fn physical_attempt_projection_rejects_unsafe_retry_predecessor() -> Result<()> {
    let first_started = started();
    let first_start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        "event-start-1",
        1,
        serde_json::to_value(&first_started)?,
        Some("event-start-1"),
        None,
    );
    let mut first_terminal = terminal(ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption);
    first_terminal.rejection = Some(crate::ProviderRequestRejection::ContextWindowExceeded);
    let first_terminal = direct_event(
        DurableEventType::ProviderPhysicalAttemptTerminal,
        "event-terminal-1",
        2,
        serde_json::to_value(first_terminal)?,
        Some("event-start-1"),
        Some("event-start-1"),
    );
    let mut second_started = started();
    second_started.physical_attempt_id = "attempt-2".to_owned();
    let second_start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        "event-start-2",
        3,
        serde_json::to_value(second_started)?,
        Some("event-start-2"),
        None,
    );

    let projection = ProviderPhysicalAttemptProjection::from_records(&[
        SessionStreamRecord::Stored(first_start),
        SessionStreamRecord::Stored(first_terminal),
        SessionStreamRecord::Stored(second_start),
    ])?;
    let error = projection
        .effective_attempt_for_logical_run_id(first_started.logical_run_id.as_str())
        .expect_err("a non-connect rejection cannot authorize another physical attempt");
    assert!(
        error
            .to_string()
            .contains("lacks durable recovery authority")
    );
    Ok(())
}

#[test]
fn physical_attempt_projection_rejects_second_terminal() -> Result<()> {
    let start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        "event-start",
        1,
        serde_json::to_value(started())?,
        Some("event-start"),
        None,
    );
    let first = direct_event(
        DurableEventType::ProviderPhysicalAttemptTerminal,
        "event-terminal-1",
        2,
        serde_json::to_value(terminal(ProviderPhysicalAttemptOutcome::Completed))?,
        Some("event-start"),
        Some("event-start"),
    );
    let second = direct_event(
        DurableEventType::ProviderPhysicalAttemptTerminal,
        "event-terminal-2",
        3,
        serde_json::to_value(terminal(ProviderPhysicalAttemptOutcome::Interrupted))?,
        Some("event-start"),
        Some("event-start"),
    );

    let error = ProviderPhysicalAttemptProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(first),
        SessionStreamRecord::Stored(second),
    ])
    .expect_err("second terminal must fail closed");
    assert!(error.to_string().contains("already has a terminal"));
    Ok(())
}

#[test]
fn physical_attempt_projection_binds_ordered_output_references() -> Result<()> {
    let start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        "event-start",
        1,
        serde_json::to_value(started())?,
        Some("event-start"),
        None,
    );
    let output = direct_event(
        DurableEventType::SessionEntryRecorded,
        "event-output",
        2,
        serde_json::json!({
            "session_log_entry": SessionLogEntry::Control(ControlEntry::UsageSnapshot(UsageStats {
                prompt_tokens: 1,
                ..UsageStats::default()
            }))
        }),
        Some("event-start"),
        Some("event-start"),
    );
    let mut terminal_entry = terminal(ProviderPhysicalAttemptOutcome::Completed);
    terminal_entry.durable_output_event_ids = vec!["event-output".to_owned()];
    let terminal = direct_event(
        DurableEventType::ProviderPhysicalAttemptTerminal,
        "event-terminal",
        3,
        serde_json::to_value(terminal_entry)?,
        Some("event-start"),
        Some("event-output"),
    );

    ProviderPhysicalAttemptProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(output),
        SessionStreamRecord::Stored(terminal),
    ])?;
    Ok(())
}

#[test]
fn physical_attempt_terminal_rejects_duplicate_or_unbounded_references() {
    let mut entry = terminal(ProviderPhysicalAttemptOutcome::Completed);
    entry.durable_output_event_ids = vec!["event-1".to_owned(), "event-1".to_owned()];
    assert!(entry.validate_shape().is_err());

    entry.durable_output_event_ids = (0..=MAX_PROVIDER_PHYSICAL_ATTEMPT_OUTPUT_REFS)
        .map(|index| format!("event-{index}"))
        .collect();
    assert!(entry.validate_shape().is_err());
}

#[test]
fn physical_attempt_rejection_requires_no_consumption_and_no_references() {
    let mut entry = terminal(ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption);
    entry.rejection = Some(crate::ProviderRequestRejection::ContextWindowExceeded);
    assert!(entry.validate_shape().is_ok());

    entry.outcome = ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain;
    assert!(entry.validate_shape().is_err());

    entry.outcome = ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption;
    entry.durable_output_event_ids = vec!["event-output".to_owned()];
    assert!(entry.validate_shape().is_err());
}

#[test]
fn physical_attempt_projection_rejects_previous_schema() -> Result<()> {
    let mut entry = started();
    entry.schema_version = 1;
    let start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        "event-start",
        1,
        serde_json::to_value(entry)?,
        Some("event-start"),
        None,
    );
    let error =
        ProviderPhysicalAttemptProjection::from_records(&[SessionStreamRecord::Stored(start)])
            .expect_err("schema v1 must not be read by the pre-release V2 contract");
    assert!(
        error
            .to_string()
            .contains("unsupported provider physical-attempt schema version 1")
    );
    Ok(())
}

#[test]
fn physical_attempt_recovery_appends_one_interrupted_terminal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let start_event_id = "event-start".to_owned();
    let start_entry = started();
    let start_payload = serde_json::to_value(&start_entry)?;
    let appended = store.append_event_if_with_identity(
        DurableEventType::ProviderPhysicalAttemptStarted,
        start_payload,
        start_event_id.clone(),
        Some(start_event_id.clone()),
        None,
        |_| Ok(true),
    )?;
    assert!(appended.is_some());

    assert_eq!(store.recover_unfinished_provider_physical_attempts(9)?, 1);
    assert_eq!(store.recover_unfinished_provider_physical_attempts(10)?, 0);

    let records = JsonlSessionStore::read_event_records(store.path())?;
    let terminal = records.iter().find_map(|record| match record {
        SessionStreamRecord::Stored(event)
            if event.event_type == DurableEventType::ProviderPhysicalAttemptTerminal.as_str() =>
        {
            serde_json::from_value::<ProviderPhysicalAttemptTerminalEntry>(event.payload.clone())
                .ok()
        }
        _ => None,
    });
    assert!(matches!(
        terminal,
        Some(ProviderPhysicalAttemptTerminalEntry {
            outcome: ProviderPhysicalAttemptOutcome::Interrupted,
            finished_at_unix_ms: 9,
            ..
        })
    ));
    Ok(())
}

#[tokio::test]
async fn non_generating_attempt_records_an_input_measurement_lifecycle() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("test-provider", "test-model").with_store(store.clone());
    let frozen = crate::FrozenProviderRequestMaterial::freeze(
        session.session_scope_id(),
        crate::CompletionRequest {
            provider_name: "test-provider".to_owned(),
            model_name: "test-model".to_owned(),
            messages: vec![crate::ModelMessage::user("count this target")],
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(128),
            reasoning_effort: None,
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: None,
            background: false,
            store: false,
            deterministic_materialization: true,
            hosted_tools: Vec::new(),
        },
    )?;

    let mut attempt = ProviderNonGeneratingAttempt::start(
        &session,
        "input-token-measurement-1",
        &frozen,
        ProviderPhysicalAttemptPurpose::InputTokenMeasurement,
    )
    .await?;
    attempt
        .finish(&session, ProviderPhysicalAttemptOutcome::Completed)
        .await?;
    assert!(attempt.completed_receipt().is_some());

    let mut failed_attempt = ProviderNonGeneratingAttempt::start(
        &session,
        "input-token-measurement-2",
        &frozen,
        ProviderPhysicalAttemptPurpose::InputTokenMeasurement,
    )
    .await?;
    failed_attempt
        .finish(
            &session,
            ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain,
        )
        .await?;
    assert!(failed_attempt.completed_receipt().is_none());

    let projection = ProviderPhysicalAttemptProjection::from_records(
        &JsonlSessionStore::read_event_records(store.path())?,
    )?;
    let attempts = projection.attempts_for_logical_run_id("input-token-measurement-1");
    assert_eq!(attempts.len(), 1);
    assert_eq!(
        attempts[0].entry.purpose,
        ProviderPhysicalAttemptPurpose::InputTokenMeasurement
    );
    let envelope = attempts[0]
        .entry
        .request_envelope
        .as_ref()
        .expect("new provider attempts must bind a request envelope");
    assert!(envelope.source_frontier.is_some());
    envelope.verify_reconstructed_request(frozen.request())?;
    let reconstruction =
        envelope.verify_reconstructed_request_at_frontier(store.path(), frozen.request())?;
    let source_frontier = envelope
        .source_frontier
        .as_ref()
        .expect("new provider attempts must bind a durable source frontier");
    assert_eq!(reconstruction.session_id, source_frontier.session_id);
    assert_eq!(
        reconstruction.source_record_count,
        source_frontier.stream_sequence.unwrap_or(0)
    );
    assert!(matches!(
        attempts[0].terminal.as_ref(),
        Some(ProviderPhysicalAttemptTerminalEntry {
            outcome: ProviderPhysicalAttemptOutcome::Completed,
            durable_output_event_ids,
            durable_side_effect_event_ids,
            ..
        }) if durable_output_event_ids.is_empty() && durable_side_effect_event_ids.is_empty()
    ));
    let first_layout = attempts[0]
        .entry
        .cache_layout_proof
        .as_ref()
        .expect("new physical attempt should persist cache layout proof");
    assert_eq!(
        first_layout.mutation_from_previous.kind,
        crate::CacheLayoutMutationKind::FirstObservation
    );
    let second_attempts = projection.attempts_for_logical_run_id("input-token-measurement-2");
    assert_eq!(
        second_attempts[0]
            .entry
            .cache_layout_proof
            .as_ref()
            .expect("second physical attempt should persist cache layout proof")
            .mutation_from_previous
            .kind,
        crate::CacheLayoutMutationKind::Identical
    );
    Ok(())
}
