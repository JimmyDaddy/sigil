use std::fs;

use anyhow::Result;

use crate::{
    ControlEntry, ConversationInputEditedEntry, ConversationInputKind,
    ConversationInputPromotedEntry, ConversationInputQueueControlAction,
    ConversationInputQueueControlEntry, ConversationInputQueueId, ConversationInputQueuedEntry,
    ConversationInputReorderedEntry, ConversationInputStatus, ConversationInputStatusEntry,
    ConversationInputTarget, ConversationQueueDurableProjection, DurableEventType, EventClass,
    JsonlSessionStore, ModelMessage, ReasoningEffort, Session, SessionLogEntry, ToolRestartPolicy,
    WebUrlCapabilityDescriptor, WebUrlProvenanceKind, conversation_promotion_capability_digest,
    project_conversation_prompt_for_persistence,
};

#[test]
fn conversation_queue_projection_preserves_fifo_and_filters_terminal_items() -> Result<()> {
    let mut session = Session::new("mock", "model");
    let first = ConversationInputQueueId::new("queue_1")?;
    let second = ConversationInputQueueId::new("queue_2")?;

    session.append_control(ControlEntry::ConversationInputQueued(queue_entry(
        first.clone(),
        "first queued prompt",
    )))?;
    session.append_control(ControlEntry::ConversationInputQueued(queue_entry(
        second.clone(),
        "second queued prompt",
    )))?;
    session.append_control(ControlEntry::ConversationInputStatusChanged(
        ConversationInputStatusEntry {
            queue_id: first.clone(),
            status: ConversationInputStatus::Dispatching,
            reason: Some("running".to_owned()),
            updated_at_ms: Some(2),
        },
    ))?;

    let projection = session.conversation_queue_projection();
    assert_eq!(projection.items.len(), 2);
    assert_eq!(projection.items[0].queued.queue_id, first);
    assert_eq!(
        projection.items[0].status,
        ConversationInputStatus::Dispatching
    );
    assert_eq!(projection.items[1].queued.queue_id, second);
    assert_eq!(projection.next_dispatchable, Some(second.clone()));

    session.append_control(ControlEntry::ConversationInputStatusChanged(
        ConversationInputStatusEntry {
            queue_id: second.clone(),
            status: ConversationInputStatus::Delivered,
            reason: None,
            updated_at_ms: Some(3),
        },
    ))?;

    let projection = session.conversation_queue_projection();
    assert_eq!(projection.items.len(), 1);
    assert_eq!(projection.items[0].queued.queue_id, first);
    assert_eq!(projection.next_dispatchable, None);
    Ok(())
}

#[test]
fn conversation_queue_projection_applies_pause_edit_and_reorder_controls() -> Result<()> {
    let mut session = Session::new("mock", "model");
    let first = ConversationInputQueueId::new("queue_1")?;
    let second = ConversationInputQueueId::new("queue_2")?;
    let missing = ConversationInputQueueId::new("queue_missing")?;

    session.append_control(ControlEntry::ConversationInputQueued(queue_entry(
        first.clone(),
        "first queued prompt",
    )))?;
    session.append_control(ControlEntry::ConversationInputQueued(queue_entry(
        second.clone(),
        "second queued prompt",
    )))?;
    session.append_control(ControlEntry::ConversationInputEdited(
        ConversationInputEditedEntry {
            queue_id: second.clone(),
            prompt_hash: "edited-hash".to_owned(),
            prompt: "edited second prompt".to_owned(),
            reasoning_effort: Some(ReasoningEffort::Low),
            updated_at_ms: Some(2),
        },
    ))?;
    session.append_control(ControlEntry::ConversationInputReordered(
        ConversationInputReorderedEntry {
            queue_id: second.clone(),
            after_queue_id: None,
            updated_at_ms: Some(3),
        },
    ))?;
    session.append_control(ControlEntry::ConversationInputReordered(
        ConversationInputReorderedEntry {
            queue_id: first.clone(),
            after_queue_id: Some(second.clone()),
            updated_at_ms: Some(4),
        },
    ))?;
    session.append_control(ControlEntry::ConversationInputReordered(
        ConversationInputReorderedEntry {
            queue_id: missing,
            after_queue_id: Some(first.clone()),
            updated_at_ms: Some(5),
        },
    ))?;
    session.append_control(ControlEntry::ConversationInputQueueControl(
        ConversationInputQueueControlEntry {
            action: ConversationInputQueueControlAction::Pause,
            reason: Some("user paused".to_owned()),
            updated_at_ms: Some(6),
        },
    ))?;

    let projection = session.conversation_queue_projection();
    assert!(projection.paused);
    assert_eq!(projection.items.len(), 2);
    assert_eq!(projection.items[0].queued.queue_id, second);
    assert_eq!(projection.items[1].queued.queue_id, first);
    assert_eq!(projection.items[0].queued.prompt, "edited second prompt");
    assert_eq!(
        projection.items[0].queued.reasoning_effort,
        Some(ReasoningEffort::Low)
    );
    assert_eq!(projection.next_dispatchable, None);

    session.append_control(ControlEntry::ConversationInputQueueControl(
        ConversationInputQueueControlEntry {
            action: ConversationInputQueueControlAction::Resume,
            reason: None,
            updated_at_ms: Some(7),
        },
    ))?;

    let projection = session.conversation_queue_projection();
    assert!(!projection.paused);
    assert_eq!(projection.next_dispatchable, Some(second));
    Ok(())
}

#[test]
fn conversation_queue_ids_validate_stable_values() {
    assert!(ConversationInputQueueId::new("queue_1").is_ok());
    assert!(ConversationInputQueueId::new("").is_err());
    assert!(ConversationInputQueueId::new("q".repeat(129)).is_err());
    assert!(ConversationInputQueueId::new("queue/1").is_err());
}

#[test]
fn queued_prompt_is_control_state_not_provider_visible_user_history() -> Result<()> {
    let mut session = Session::new("mock", "model");
    session.append_control(ControlEntry::ConversationInputQueued(queue_entry(
        ConversationInputQueueId::new("queue_1")?,
        "queued but not dispatched",
    )))?;

    assert!(
        session
            .entries()
            .iter()
            .all(|entry| !matches!(entry, SessionLogEntry::User(_)))
    );
    assert_eq!(session.conversation_queue_projection().items.len(), 1);
    Ok(())
}

#[test]
fn direct_promotion_atomically_binds_a_safe_message_and_replays_after_reload() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let queue_id = ConversationInputQueueId::new("queue_promote_1")?;
    let raw_prompt = "open https://example.com/private?token=raw-promotion-secret";
    let queued = durable_queue_entry(queue_id.clone(), raw_prompt);
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ConversationInputQueued(queued.clone()),
    ))?;

    let records = store.read_event_records_writer()?;
    let durable = ConversationQueueDurableProjection::from_records(&records)?;
    let session_id = records
        .last()
        .expect("queued entry must have a durable record")
        .session_id()
        .to_owned();
    let promoted = promotion_entry(
        queue_id.clone(),
        durable
            .revision
            .expect("queued entry must advance queue revision"),
        queued.prompt_hash.clone(),
        queued.prompt.clone(),
        true,
    )?;

    let event = store.append_conversation_input_promoted(promoted.clone())?;
    assert_eq!(event.event_type, "conversation_input_promoted");
    assert_eq!(event.session_id, session_id);
    let content = fs::read_to_string(&path)?;
    assert!(!content.contains("raw-promotion-secret"));

    let restored = Session::load_from_store("mock", "model", store.clone())?;
    assert!(restored.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(entry))
            if entry.queue_id == queue_id
                && entry.durable_user_message.id == promoted.durable_user_message.id
    )));
    assert!(
        restored
            .entries()
            .iter()
            .all(|entry| !matches!(entry, SessionLogEntry::User(_)))
    );
    let queue = restored
        .try_conversation_queue_projection_from_durable()?
        .expect("store-backed session must replay queue state");
    assert_eq!(queue.next_dispatchable, None);
    assert_eq!(queue.items.len(), 1);
    assert_eq!(queue.items[0].status, ConversationInputStatus::Dispatching);
    assert_eq!(queue.items[0].reason.as_deref(), Some("promotion_bound"));

    let replayed =
        ConversationQueueDurableProjection::from_records(&store.read_event_records_writer()?)?;
    assert_eq!(
        replayed
            .revision
            .expect("promotion must advance queue revision")
            .event_id,
        event.event_id
    );
    Ok(())
}

#[test]
fn promotion_compare_and_swap_rejects_stale_or_duplicate_append_without_writing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let queue_id = ConversationInputQueueId::new("queue_promote_2")?;
    let queued = durable_queue_entry(queue_id.clone(), "safe queued prompt");
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ConversationInputQueued(queued.clone()),
    ))?;
    let durable =
        ConversationQueueDurableProjection::from_records(&store.read_event_records_writer()?)?;
    let promoted = promotion_entry(
        queue_id.clone(),
        durable
            .revision
            .expect("queued entry must advance queue revision"),
        queued.prompt_hash.clone(),
        queued.prompt.clone(),
        false,
    )?;

    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ConversationInputQueueControl(ConversationInputQueueControlEntry {
            action: ConversationInputQueueControlAction::Pause,
            reason: Some("user paused".to_owned()),
            updated_at_ms: Some(2),
        }),
    ))?;
    let before_stale_attempt = fs::read(&path)?;
    assert!(
        store
            .append_conversation_input_promoted(promoted.clone())
            .is_err()
    );
    assert_eq!(fs::read(&path)?, before_stale_attempt);

    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ConversationInputQueueControl(ConversationInputQueueControlEntry {
            action: ConversationInputQueueControlAction::Resume,
            reason: None,
            updated_at_ms: Some(3),
        }),
    ))?;
    let revised =
        ConversationQueueDurableProjection::from_records(&store.read_event_records_writer()?)?;
    let current = promotion_entry(
        queue_id,
        revised
            .revision
            .expect("resume must advance queue revision"),
        queued.prompt_hash,
        queued.prompt,
        false,
    )?;
    store.append_conversation_input_promoted(current.clone())?;
    let before_duplicate_attempt = fs::read(&path)?;
    assert!(store.append_conversation_input_promoted(current).is_err());
    assert_eq!(fs::read(&path)?, before_duplicate_attempt);
    Ok(())
}

#[test]
fn malformed_direct_promotion_payload_fails_closed_on_session_load() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let queue_id = ConversationInputQueueId::new("queue_promote_3")?;
    let queued = durable_queue_entry(queue_id.clone(), "safe queued prompt");
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ConversationInputQueued(queued.clone()),
    ))?;
    let durable =
        ConversationQueueDurableProjection::from_records(&store.read_event_records_writer()?)?;
    let promotion = promotion_entry(
        queue_id,
        durable
            .revision
            .expect("queued entry must advance queue revision"),
        queued.prompt_hash,
        queued.prompt,
        false,
    )?;
    let mut malformed = serde_json::to_value(promotion)?;
    malformed
        .as_object_mut()
        .expect("promotion must be an object")
        .insert("unexpected_field".to_owned(), serde_json::json!(true));
    store.append_event(
        DurableEventType::ConversationInputPromoted,
        EventClass::Critical,
        malformed,
    )?;

    let error = Session::load_from_store("mock", "model", store)
        .expect_err("unknown promotion field must reject session reload");
    assert!(format!("{error:#}").contains("unknown field"));
    Ok(())
}

#[test]
fn stale_direct_promotion_record_fails_closed_on_session_load() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let queue_id = ConversationInputQueueId::new("queue_promote_5")?;
    let queued = durable_queue_entry(queue_id.clone(), "safe queued prompt");
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ConversationInputQueued(queued.clone()),
    ))?;
    let durable =
        ConversationQueueDurableProjection::from_records(&store.read_event_records_writer()?)?;
    let mut promotion = promotion_entry(
        queue_id,
        durable
            .revision
            .expect("queued entry must advance queue revision"),
        queued.prompt_hash,
        queued.prompt,
        false,
    )?;
    promotion.expected_queue_revision.stream_sequence = 999;
    store.append_event(
        DurableEventType::ConversationInputPromoted,
        EventClass::Critical,
        serde_json::to_value(promotion)?,
    )?;

    let error = Session::load_from_store("mock", "model", store)
        .expect_err("stale promotion must reject session reload");
    assert!(format!("{error:#}").contains("queue revision is stale"));
    Ok(())
}

#[test]
fn promotion_rejects_noncanonical_or_cross_session_capability_descriptors() -> Result<()> {
    let message = ModelMessage::user("safe queued prompt");
    let prompt = project_conversation_prompt_for_persistence(
        message
            .content
            .as_deref()
            .expect("user message has content"),
    );
    let mut entry = ConversationInputPromotedEntry {
        queue_id: ConversationInputQueueId::new("queue_promote_4")?,
        expected_queue_revision: crate::ConversationQueueRevision {
            stream_sequence: 1,
            event_id: "event-1".to_owned(),
        },
        prompt_hash: prompt.prompt_hash,
        exact_prompt_required: false,
        durable_user_message: message.clone(),
        capability_descriptors: vec![
            capability_descriptor(
                "session-1",
                "src_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                &message.id,
            ),
            capability_descriptor(
                "session-1",
                "src_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &message.id,
            ),
        ],
        capability_digest: String::new(),
        dispatch_run_id: "run-promote-4".to_owned(),
        promoted_at_ms: 1,
    };
    entry.capability_digest =
        conversation_promotion_capability_digest(&entry.capability_descriptors)?;
    assert!(entry.validate_shape().is_err());

    entry.capability_descriptors.swap(0, 1);
    entry.capability_digest =
        conversation_promotion_capability_digest(&entry.capability_descriptors)?;
    assert!(entry.validate_for_session("session-2").is_err());
    assert!(entry.validate_for_session("session-1").is_ok());
    Ok(())
}

fn queue_entry(
    queue_id: ConversationInputQueueId,
    prompt: impl Into<String>,
) -> ConversationInputQueuedEntry {
    ConversationInputQueuedEntry {
        queue_id,
        target: ConversationInputTarget::MainThread,
        kind: ConversationInputKind::Chat,
        prompt_hash: "hash".to_owned(),
        prompt: prompt.into(),
        reasoning_effort: None,
        created_at_ms: Some(1),
    }
}

fn durable_queue_entry(
    queue_id: ConversationInputQueueId,
    raw_prompt: &str,
) -> ConversationInputQueuedEntry {
    let projection = project_conversation_prompt_for_persistence(raw_prompt);
    ConversationInputQueuedEntry {
        queue_id,
        target: ConversationInputTarget::MainThread,
        kind: ConversationInputKind::Chat,
        prompt_hash: projection.prompt_hash,
        prompt: projection.safe_prompt,
        reasoning_effort: None,
        created_at_ms: Some(1),
    }
}

fn promotion_entry(
    queue_id: ConversationInputQueueId,
    expected_queue_revision: crate::ConversationQueueRevision,
    prompt_hash: String,
    safe_prompt: String,
    exact_prompt_required: bool,
) -> Result<ConversationInputPromotedEntry> {
    Ok(ConversationInputPromotedEntry {
        queue_id,
        expected_queue_revision,
        prompt_hash,
        exact_prompt_required,
        durable_user_message: ModelMessage::user(safe_prompt),
        capability_descriptors: Vec::new(),
        capability_digest: conversation_promotion_capability_digest(&[])?,
        dispatch_run_id: "run-promote-1".to_owned(),
        promoted_at_ms: 1,
    })
}

fn capability_descriptor(
    session_scope_id: &str,
    source_id: &str,
    durable_entry_id: &str,
) -> WebUrlCapabilityDescriptor {
    WebUrlCapabilityDescriptor {
        session_scope_id: session_scope_id.to_owned(),
        source_id: source_id.to_owned(),
        durable_entry_id: durable_entry_id.to_owned(),
        safe_display_url: "https://example.com/".to_owned(),
        restart_policy: ToolRestartPolicy::Replayable,
        replayable_canonical_url: Some("https://example.com/".to_owned()),
        originating_call_id: None,
        provenance: WebUrlProvenanceKind::UserMessage,
        issued_at_ms: 1,
        expires_at_ms: 2,
    }
}
