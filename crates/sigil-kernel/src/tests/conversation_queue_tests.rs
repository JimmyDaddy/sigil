use anyhow::Result;

use crate::{
    ControlEntry, ConversationInputEditedEntry, ConversationInputKind,
    ConversationInputQueueControlAction, ConversationInputQueueControlEntry,
    ConversationInputQueueId, ConversationInputQueuedEntry, ConversationInputReorderedEntry,
    ConversationInputStatus, ConversationInputStatusEntry, ConversationInputTarget,
    ReasoningEffort, Session, SessionLogEntry,
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
