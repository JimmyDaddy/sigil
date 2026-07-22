use std::collections::HashSet;

use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use sigil_kernel::{
    AssistantMessageKind, ConversationRunFinalizedEntryV1, ConversationRunStartedEntryV1,
    ConversationRunTerminalStatusV1, DurableEventType, EventClass, JsonlSessionStore, MessageRole,
    ModelMessage, SecretRedactor, Session, SessionLogEntry, SessionStreamRecord, StoredEvent,
    ToolCall,
};

use crate::conversation_display::{
    ConversationDisplayAssistantPhaseV1, ConversationDisplayContentV1,
    ConversationDisplayItemKindV1, ConversationDisplayMessageRoleV1, ConversationDisplayStatusV1,
    MAX_CONVERSATION_DISPLAY_CONTENT_BYTES, MAX_CONVERSATION_DISPLAY_PAGE_BYTES,
    MAX_CONVERSATION_DISPLAY_PAGE_SIZE, conversation_display_page,
    conversation_display_page_from_records,
};

fn durable_session() -> Result<(tempfile::TempDir, JsonlSessionStore, Session)> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("provider", "model").with_store(store.clone());
    Ok((temp, store, session))
}

#[test]
fn canonical_projection_has_stable_ids_orders_and_run_binding() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let recorder = session.conversation_run_lifecycle_recorder()?;
    recorder.append_started(&ConversationRunStartedEntryV1::new("run-1", 10)?)?;

    session.append_user_message(ModelMessage::user("inspect this"))?;
    let tool_call = ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: r#"{"path":"secret"}"#.to_owned(),
    };
    let final_message = ModelMessage::assistant_with_kind(
        Some("done".to_owned()),
        vec![tool_call],
        AssistantMessageKind::FinalAnswer,
    );
    let final_message_id = final_message.id.clone();
    session.append_assistant_message(final_message)?;
    session.append_tool_message(ModelMessage::tool("call-1", "file output"))?;
    recorder.append_finalized(&ConversationRunFinalizedEntryV1::new(
        "run-1",
        ConversationRunTerminalStatusV1::Succeeded,
        Some(final_message_id),
        Some("complete"),
        20,
        &SecretRedactor::empty(),
    )?)?;

    let first = conversation_display_page(store.path(), &scope, None, 20)?;
    let second = conversation_display_page(store.path(), &scope, None, 20)?;
    assert_eq!(first, second);
    assert_eq!(first.items.len(), 5);
    assert!(
        first
            .items
            .windows(2)
            .all(|items| items[0].display_order < items[1].display_order)
    );
    assert_eq!(
        first
            .items
            .iter()
            .map(|item| item.display_id.as_str())
            .collect::<HashSet<_>>()
            .len(),
        first.items.len()
    );
    assert!(
        first
            .items
            .iter()
            .all(|item| item.run_id.as_deref() == Some("run-1"))
    );
    assert!(first.items.iter().all(|item| item.run_sequence.is_none()));
    assert!(
        first
            .items
            .iter()
            .all(|item| !item.source_event_id.is_empty())
    );
    assert_eq!(
        first
            .items
            .iter()
            .filter(|item| item.kind == ConversationDisplayItemKindV1::Terminal)
            .count(),
        1
    );
    assert_eq!(
        first
            .items
            .iter()
            .filter(|item| {
                matches!(
                    item.content,
                    ConversationDisplayContentV1::Message {
                        assistant_phase: Some(ConversationDisplayAssistantPhaseV1::FinalAnswer),
                        ..
                    }
                )
            })
            .count(),
        1,
        "terminal evidence must not duplicate the final assistant answer"
    );
    assert_eq!(
        first
            .terminal_frontier
            .as_ref()
            .map(|frontier| (frontier.run_id.as_str(), frontier.status,)),
        Some(("run-1", ConversationDisplayStatusV1::Succeeded))
    );
    Ok(())
}

#[test]
fn terminal_must_match_the_unique_durable_final_for_its_active_run() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let recorder = session.conversation_run_lifecycle_recorder()?;
    recorder.append_started(&ConversationRunStartedEntryV1::new("run-1", 10)?)?;
    let final_message = ModelMessage::assistant_with_kind(
        Some("durable answer".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    );
    session.append_assistant_message(final_message)?;
    recorder.append_finalized(&ConversationRunFinalizedEntryV1::new(
        "run-1",
        ConversationRunTerminalStatusV1::Succeeded,
        Some("another-message".to_owned()),
        Some("complete"),
        20,
        &SecretRedactor::empty(),
    )?)?;

    assert!(
        conversation_display_page(store.path(), &scope, None, 20)
            .expect_err("succeeded terminal must bind the active run's durable final")
            .to_string()
            .contains("does not match")
    );

    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let recorder = session.conversation_run_lifecycle_recorder()?;
    recorder.append_started(&ConversationRunStartedEntryV1::new("run-2", 30)?)?;
    session.append_assistant_message(ModelMessage::assistant_with_kind(
        Some("first final".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    ))?;
    session.append_assistant_message(ModelMessage::assistant_with_kind(
        Some("second final".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    ))?;
    assert!(
        conversation_display_page(store.path(), &scope, None, 20)
            .expect_err("one run cannot project two durable final assistants")
            .to_string()
            .contains("more than one")
    );
    Ok(())
}

#[test]
fn legacy_messages_remain_unbound_and_do_not_synthesize_terminal_items() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    session.append_user_message(ModelMessage::user("legacy user"))?;
    session.append_assistant_message(ModelMessage::assistant_with_kind(
        Some("legacy answer".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    ))?;
    store.append_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({
            "run_status": "completed",
            "terminal_reason": "completed",
            "final_message_id": null,
            "tool_calls": 0,
            "error": null
        }),
    )?;

    let page = conversation_display_page(store.path(), &scope, None, 20)?;
    assert_eq!(page.items.len(), 2);
    assert!(page.items.iter().all(|item| item.run_id.is_none()));
    assert!(
        page.items
            .iter()
            .all(|item| item.kind != ConversationDisplayItemKindV1::Terminal)
    );
    assert!(page.terminal_frontier.is_none());
    Ok(())
}

#[test]
fn cursor_pins_a_fixed_frontier_while_new_history_is_appended() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    for index in 0..5 {
        session.append_user_message(ModelMessage::user(format!("message-{index}")))?;
    }

    let first = conversation_display_page(store.path(), &scope, None, 2)?;
    assert_eq!(first.items.len(), 2);
    assert!(first.has_more);
    let cursor = first.next_cursor.clone().expect("older page cursor");
    let decoded_cursor = String::from_utf8(URL_SAFE_NO_PAD.decode(&cursor)?)?;
    assert!(!decoded_cursor.contains(&scope));
    for record in JsonlSessionStore::read_event_records(store.path())? {
        assert!(!decoded_cursor.contains(record.event_id()));
        assert!(!decoded_cursor.contains(record.record_checksum()));
    }
    let mut forged_payload: serde_json::Value = serde_json::from_str(&decoded_cursor)?;
    forged_payload["before_order"]["subindex"] = json!(99);
    let forged_cursor = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged_payload)?);
    assert!(
        conversation_display_page(store.path(), &scope, Some(&forged_cursor), 2)
            .expect_err("a re-encoded cursor boundary must not be forgeable")
            .to_string()
            .contains("frontier")
    );
    let first_ids = first
        .items
        .iter()
        .map(|item| item.display_id.clone())
        .collect::<HashSet<_>>();

    session.append_user_message(ModelMessage::user("new-after-frontier"))?;
    let second = conversation_display_page(store.path(), &scope, Some(&cursor), 2)?;
    assert_eq!(
        second.through_session_stream_sequence,
        first.through_session_stream_sequence
    );
    assert_eq!(second.total_items, 5);
    assert!(
        second
            .items
            .iter()
            .all(|item| !first_ids.contains(&item.display_id))
    );
    assert!(second.items.iter().all(|item| {
        !matches!(
            &item.content,
            ConversationDisplayContentV1::Message { text: Some(text), .. }
                if text == "new-after-frontier"
        )
    }));

    assert!(
        conversation_display_page(store.path(), "another-scope", Some(&cursor), 2)
            .expect_err("scope-bound cursor must fail")
            .to_string()
            .contains("scope")
    );
    let mut tampered = cursor;
    tampered.push('x');
    assert!(conversation_display_page(store.path(), &scope, Some(&tampered), 2).is_err());
    Ok(())
}

#[test]
fn projection_is_secret_safe_and_bounded_by_item_page_and_limit() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let large_content = "x".repeat(70_000);
    for _ in 0..12 {
        session.append_user_message(ModelMessage::user(large_content.clone()))?;
    }
    session.append_user_message(ModelMessage::user("token=sk-test-secret"))?;

    let page = conversation_display_page(store.path(), &scope, None, 12)?;
    assert!(
        page.has_more,
        "page byte budget should preserve an older cursor"
    );
    assert!(serde_json::to_vec(&page.items)?.len() <= MAX_CONVERSATION_DISPLAY_PAGE_BYTES);
    for item in &page.items {
        let ConversationDisplayContentV1::Message {
            text: Some(text),
            truncated,
            original_content_bytes,
            ..
        } = &item.content
        else {
            panic!("expected message content");
        };
        assert!(text.len() <= MAX_CONVERSATION_DISPLAY_CONTENT_BYTES);
        if *original_content_bytes == large_content.len() {
            assert!(*truncated);
        }
        assert!(!text.contains("sk-"));
    }
    assert!(conversation_display_page(store.path(), &scope, None, 0).is_err());
    assert!(
        conversation_display_page(
            store.path(),
            &scope,
            None,
            MAX_CONVERSATION_DISPLAY_PAGE_SIZE + 1,
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn reasoning_is_typed_and_empty_messages_do_not_create_placeholders() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    session.append_assistant_message(ModelMessage::assistant_with_kind(
        Some("reasoning details".to_owned()),
        Vec::new(),
        AssistantMessageKind::ReasoningTrace,
    ))?;
    session.append_user_message(ModelMessage::new(MessageRole::User, None))?;

    let page = conversation_display_page(store.path(), &scope, None, 20)?;
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].kind, ConversationDisplayItemKindV1::Reasoning);
    assert!(matches!(
        &page.items[0].content,
        ConversationDisplayContentV1::Reasoning { text, .. } if text == "reasoning details"
    ));
    Ok(())
}

#[test]
fn unknown_critical_lifecycle_and_checksum_tampering_fail_closed() -> Result<()> {
    let unknown = SessionStreamRecord::Stored(StoredEvent::new_raw(
        "future_critical_event",
        EventClass::Critical,
        "event-1".to_owned(),
        "scope-1".to_owned(),
        1,
        json!({"future": true}),
    )?);
    assert!(
        conversation_display_page_from_records(&[unknown], "scope-1", None, 10)
            .expect_err("unknown critical event must fail")
            .to_string()
            .contains("unknown critical")
    );

    let future_lifecycle = SessionStreamRecord::Stored(StoredEvent::new(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        "event-1".to_owned(),
        "scope-1".to_owned(),
        1,
        json!({"record": "conversation_run_started_v2"}),
    )?);
    assert!(
        conversation_display_page_from_records(&[future_lifecycle], "scope-1", None, 10)
            .expect_err("future critical lifecycle tag must fail")
            .to_string()
            .contains("unknown critical run lifecycle")
    );

    let mut tampered = StoredEvent::new(
        DurableEventType::UserMessageRecorded,
        EventClass::Critical,
        "event-1".to_owned(),
        "scope-1".to_owned(),
        1,
        json!({"session_log_entry": SessionLogEntry::User(ModelMessage::user("hello"))}),
    )?;
    tampered.record_checksum.push('0');
    assert!(
        conversation_display_page_from_records(
            &[SessionStreamRecord::Stored(tampered)],
            "scope-1",
            None,
            10,
        )
        .expect_err("tampered checksum must fail")
        .to_string()
        .contains("checksum")
    );
    Ok(())
}

#[test]
fn role_mismatch_and_overlapping_runs_fail_closed() -> Result<()> {
    let mismatched = StoredEvent::new(
        DurableEventType::UserMessageRecorded,
        EventClass::Critical,
        "event-1".to_owned(),
        "scope-1".to_owned(),
        1,
        json!({
            "session_log_entry": SessionLogEntry::User(ModelMessage::assistant(
                Some("wrong role".to_owned()),
                Vec::new(),
            ))
        }),
    )?;
    assert!(
        conversation_display_page_from_records(
            &[SessionStreamRecord::Stored(mismatched)],
            "scope-1",
            None,
            10,
        )
        .expect_err("role mismatch must fail")
        .to_string()
        .contains("non-user role")
    );

    let start_one = ConversationRunStartedEntryV1::new("run-1", 1)?;
    let start_two = ConversationRunStartedEntryV1::new("run-2", 2)?;
    let records = vec![
        SessionStreamRecord::Stored(StoredEvent::new(
            DurableEventType::RunStatusChanged,
            EventClass::Critical,
            "event-1".to_owned(),
            "scope-1".to_owned(),
            1,
            serde_json::to_value(
                sigil_kernel::ConversationRunLifecycleRecordV1::ConversationRunStartedV1(start_one),
            )?,
        )?),
        SessionStreamRecord::Stored(StoredEvent::new(
            DurableEventType::RunStatusChanged,
            EventClass::Critical,
            "event-2".to_owned(),
            "scope-1".to_owned(),
            2,
            serde_json::to_value(
                sigil_kernel::ConversationRunLifecycleRecordV1::ConversationRunStartedV1(start_two),
            )?,
        )?),
    ];
    assert!(
        conversation_display_page_from_records(&records, "scope-1", None, 10)
            .expect_err("overlapping runs must fail")
            .to_string()
            .contains("overlapping")
    );
    Ok(())
}

#[test]
fn message_content_role_remains_provider_neutral() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    session.append_user_message(ModelMessage::user("hello"))?;
    let page = conversation_display_page(store.path(), &scope, None, 1)?;
    assert!(matches!(
        page.items[0].content,
        ConversationDisplayContentV1::Message {
            role: ConversationDisplayMessageRoleV1::User,
            ..
        }
    ));
    Ok(())
}
