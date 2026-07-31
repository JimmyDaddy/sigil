use super::*;

#[test]
fn source_span_rejects_a_record_checksum_mismatch() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("source-span.jsonl"))?;
    let mut session = Session::new("mock", "model").with_store(store.clone());
    session.append_user_message(ModelMessage::user("exact objective"))?;
    let records = store.read_event_records_writer()?;
    let mut source = SourceSpanRefV1::from_record(
        &records[0],
        "session_log_entry.user.content".to_owned(),
        b"exact objective",
        session.messages().first().map(|message| message.id.clone()),
    )?;
    source.validate_against_records(&records)?;

    source.record_checksum = "sha256:jcs-v1:tampered".to_owned();
    assert!(
        source
            .validate_against_records(&records)
            .expect_err("record checksum drift must fail closed")
            .to_string()
            .contains("does not match its durable event")
    );
    Ok(())
}

#[test]
fn whole_event_source_binds_the_record_checksum_not_generated_text() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("whole-event-source.jsonl"))?;
    let mut session = Session::new("mock", "model").with_store(store.clone());
    session.append_user_message(ModelMessage::user("durable source"))?;
    let records = store.read_event_records_writer()?;
    let mut source = SourceSpanRefV1::from_whole_event(&records[0], None)?;

    assert_eq!(source.field_path, WHOLE_EVENT_SOURCE_PATH);
    source.validate_against_records(&records)?;
    source.cited_value_hash = cited_value_hash(b"model-generated narrative");
    assert!(
        source
            .validate_against_records(&records)
            .expect_err("whole-event source must stay checksum-bound")
            .to_string()
            .contains("whole-event reference")
    );
    Ok(())
}

#[test]
fn legacy_anchor_rejects_a_hash_bound_value_with_the_wrong_field_path() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("legacy-anchor.jsonl"))?;
    let mut session = Session::new("mock", "model").with_store(store.clone());
    session.append_user_message(ModelMessage::user("exact objective"))?;
    let records = store.read_event_records_writer()?;
    let messages = session.messages();
    let message = messages.first().context("durable user message")?;
    let statement = AnchoredStatementV1 {
        exact_text: "exact objective".to_owned(),
        authority: ObjectiveAuthorityRefV1::UserSourceTurn {
            event_id: records[0].event_id().to_owned(),
            message_id: message.id.clone(),
        },
        source: SourceSpanRefV1::from_record(
            &records[0],
            "session_log_entry.assistant.content".to_owned(),
            b"exact objective",
            Some(message.id.clone()),
        )?,
    };

    assert!(
        statement
            .validate_against_records(&records)
            .expect_err("field-path drift must fail closed")
            .to_string()
            .contains("exact durable text span")
    );
    Ok(())
}

#[test]
fn grounded_item_rejects_text_not_present_in_its_exact_durable_field() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("grounded-item.jsonl"))?;
    let mut session = Session::new("mock", "model").with_store(store.clone());
    session.append_user_message(ModelMessage::user("durable fact"))?;
    let records = store.read_event_records_writer()?;
    let source =
        required_exact_string_source_for_event_id(&records, records[0].event_id(), "durable fact")?;
    let fabricated = GroundedContinuityItemV2 {
        text: "fabricated fact".to_owned(),
        source_refs: vec![source],
        artifact_ref: None,
        receipt_ref: None,
    };

    assert!(
        fabricated
            .validate_against_records(&records, true)
            .expect_err("fabricated grounded text must fail closed")
            .to_string()
            .contains("does not match an exact cited durable field")
    );
    Ok(())
}
