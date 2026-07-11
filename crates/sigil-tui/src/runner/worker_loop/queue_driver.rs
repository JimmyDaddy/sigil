use super::*;

const MAX_EXACT_CONVERSATION_PROMPTS: usize = 128;
const EXACT_PROMPT_REQUIRED_HASH_PREFIX: &str = "exact-required:";

pub(in crate::runner) type ExactConversationPromptStore =
    BTreeMap<ConversationInputQueueId, SecretString>;

pub(in crate::runner) fn queue_conversation_input(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    exact_prompts: &mut ExactConversationPromptStore,
    prompt: String,
    kind: ConversationInputKind,
    target: ConversationInputTarget,
    reasoning_effort: ReasoningEffort,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    let entries = current_session
        .as_ref()
        .map(|session| session.entries().to_vec())
        .unwrap_or_else(|| JsonlSessionStore::read_entries(session_log_path).unwrap_or_default());
    let active_queue_count = ConversationQueueProjection::from_entries(&entries)
        .items
        .len();
    if active_queue_count >= MAX_EXACT_CONVERSATION_PROMPTS
        || exact_prompts.len() >= MAX_EXACT_CONVERSATION_PROMPTS
    {
        return Err("conversation input queue capacity is exhausted".to_owned());
    }
    let queue_id = next_conversation_queue_id(&entries)?;
    let safe_prompt = sigil_kernel::safe_persistence_text(&prompt);
    let prompt_hash = durable_conversation_prompt_hash(&prompt, &safe_prompt);
    let entry = ConversationInputQueuedEntry {
        queue_id: queue_id.clone(),
        target,
        kind,
        prompt_hash,
        prompt: safe_prompt,
        reasoning_effort: Some(reasoning_effort),
        created_at_ms: Some(current_unix_time_ms()),
    };
    let control = ControlEntry::ConversationInputQueued(entry);
    if let Some(session) = current_session.as_mut() {
        session
            .append_control(control)
            .map_err(|error| format!("failed to append follow-up: {error:#}"))?;
        exact_prompts.insert(queue_id, SecretString::new(prompt));
        Ok(session.entries().to_vec())
    } else {
        let store = JsonlSessionStore::new(session_log_path.to_path_buf())
            .map_err(|error| format!("failed to open session store for follow-up: {error:#}"))?;
        store
            .append(&SessionLogEntry::Control(control))
            .map_err(|error| format!("failed to persist follow-up: {error:#}"))?;
        exact_prompts.insert(queue_id, SecretString::new(prompt));
        JsonlSessionStore::read_entries(session_log_path)
            .map_err(|error| format!("failed to reload follow-up: {error:#}"))
    }
}

pub(in crate::runner) fn cancel_queued_conversation_input(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    exact_prompts: &mut ExactConversationPromptStore,
    queue_id: ConversationInputQueueId,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    ensure_queued_conversation_item_is_mutable(session_log_path, current_session, &queue_id)?;
    let entries = append_conversation_queue_control_entries(
        session_log_path,
        current_session,
        vec![ControlEntry::ConversationInputStatusChanged(
            ConversationInputStatusEntry {
                queue_id: queue_id.clone(),
                status: ConversationInputStatus::Cancelled,
                reason: Some("cancelled by user".to_owned()),
                updated_at_ms: Some(current_unix_time_ms()),
            },
        )],
    )?;
    exact_prompts.remove(&queue_id);
    Ok(entries)
}

pub(in crate::runner) fn edit_queued_conversation_input(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    exact_prompts: &mut ExactConversationPromptStore,
    queue_id: ConversationInputQueueId,
    prompt: String,
    reasoning_effort: ReasoningEffort,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    if prompt.trim().is_empty() {
        return Err("follow-up prompt cannot be empty".to_owned());
    }
    ensure_queued_conversation_item_is_mutable(session_log_path, current_session, &queue_id)?;
    let safe_prompt = sigil_kernel::safe_persistence_text(&prompt);
    let prompt_hash = durable_conversation_prompt_hash(&prompt, &safe_prompt);
    let entries = append_conversation_queue_control_entries(
        session_log_path,
        current_session,
        vec![ControlEntry::ConversationInputEdited(
            ConversationInputEditedEntry {
                queue_id: queue_id.clone(),
                prompt_hash,
                prompt: safe_prompt,
                reasoning_effort: Some(reasoning_effort),
                updated_at_ms: Some(current_unix_time_ms()),
            },
        )],
    )?;
    exact_prompts.insert(queue_id, SecretString::new(prompt));
    Ok(entries)
}

pub(in crate::runner) fn move_queued_conversation_input(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    queue_id: ConversationInputQueueId,
    direction: QueueMoveDirection,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    let entries = read_conversation_queue_entries(session_log_path, current_session)?;
    let projection = ConversationQueueProjection::from_entries(&entries);
    ensure_projection_item_is_mutable(&projection, &queue_id)?;
    let Some(index) = projection
        .items
        .iter()
        .position(|item| item.queued.queue_id == queue_id)
    else {
        return Err(format!("follow-up {} not found", queue_id.as_str()));
    };
    let after_queue_id = match direction {
        QueueMoveDirection::Up if index == 0 => return Ok(entries),
        QueueMoveDirection::Up if index == 1 => None,
        QueueMoveDirection::Up => Some(projection.items[index - 2].queued.queue_id.clone()),
        QueueMoveDirection::Down if index + 1 >= projection.items.len() => return Ok(entries),
        QueueMoveDirection::Down => Some(projection.items[index + 1].queued.queue_id.clone()),
    };
    append_conversation_queue_control_entries(
        session_log_path,
        current_session,
        vec![ControlEntry::ConversationInputReordered(
            ConversationInputReorderedEntry {
                queue_id,
                after_queue_id,
                updated_at_ms: Some(current_unix_time_ms()),
            },
        )],
    )
}

pub(in crate::runner) fn promote_queued_conversation_input(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    queue_id: ConversationInputQueueId,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    let entries = read_conversation_queue_entries(session_log_path, current_session)?;
    let projection = ConversationQueueProjection::from_entries(&entries);
    ensure_projection_item_is_mutable(&projection, &queue_id)?;
    let mut controls = Vec::new();
    if projection.paused {
        controls.push(ControlEntry::ConversationInputQueueControl(
            ConversationInputQueueControlEntry {
                action: ConversationInputQueueControlAction::Resume,
                reason: Some("next turn".to_owned()),
                updated_at_ms: Some(current_unix_time_ms()),
            },
        ));
    }
    controls.push(ControlEntry::ConversationInputReordered(
        ConversationInputReorderedEntry {
            queue_id,
            after_queue_id: None,
            updated_at_ms: Some(current_unix_time_ms()),
        },
    ));
    append_conversation_queue_control_entries(session_log_path, current_session, controls)
}

pub(in crate::runner) fn set_conversation_queue_paused(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    paused: bool,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    append_conversation_queue_control_entries(
        session_log_path,
        current_session,
        vec![ControlEntry::ConversationInputQueueControl(
            ConversationInputQueueControlEntry {
                action: if paused {
                    ConversationInputQueueControlAction::Pause
                } else {
                    ConversationInputQueueControlAction::Resume
                },
                reason: Some("user control".to_owned()),
                updated_at_ms: Some(current_unix_time_ms()),
            },
        )],
    )
}

pub(in crate::runner) fn ensure_queued_conversation_item_is_mutable(
    session_log_path: &Path,
    current_session: &Option<Session>,
    queue_id: &ConversationInputQueueId,
) -> std::result::Result<(), String> {
    let entries = read_conversation_queue_entries(session_log_path, current_session)?;
    let projection = ConversationQueueProjection::from_entries(&entries);
    ensure_projection_item_is_mutable(&projection, queue_id)
}

pub(in crate::runner) fn ensure_projection_item_is_mutable(
    projection: &ConversationQueueProjection,
    queue_id: &ConversationInputQueueId,
) -> std::result::Result<(), String> {
    let Some(item) = projection
        .items
        .iter()
        .find(|item| item.queued.queue_id == *queue_id)
    else {
        return Err(format!("follow-up {} not found", queue_id.as_str()));
    };
    if item.status != ConversationInputStatus::Queued {
        return Err(format!(
            "follow-up {} is already {}",
            queue_id.as_str(),
            queue_status_label(item.status)
        ));
    }
    Ok(())
}

pub(in crate::runner) fn append_conversation_queue_control_entries(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    controls: Vec<ControlEntry>,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    append_session_control_entries(
        session_log_path,
        current_session,
        controls,
        "conversation queue",
    )
    .map_err(|error| format!("{error:#}"))
}

pub(in crate::runner) fn append_agent_result_continuation_status_entries(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    thread_ids: &[AgentThreadId],
    status: AgentResultContinuationStatus,
    reason: Option<&str>,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    let controls = thread_ids
        .iter()
        .cloned()
        .map(|thread_id| {
            ControlEntry::AgentResultContinuation(AgentResultContinuationEntry {
                thread_id,
                status,
                reason: reason.map(str::to_owned),
                updated_at_ms: Some(current_unix_time_ms()),
            })
        })
        .collect::<Vec<_>>();
    append_session_control_entries(
        session_log_path,
        current_session,
        controls,
        "agent result continuation",
    )
    .map_err(|error| format!("{error:#}"))
}

pub(in crate::runner) fn append_agent_result_continuation_status_and_notify(
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    thread_ids: &[AgentThreadId],
    status: AgentResultContinuationStatus,
    reason: Option<&str>,
) {
    let Some(session) = current_session.as_mut() else {
        let _ = message_tx.send(WorkerMessage::Notice(
            "agent result continuation status skipped: session state unavailable".to_owned(),
        ));
        return;
    };
    for thread_id in thread_ids {
        let entry = AgentResultContinuationEntry {
            thread_id: thread_id.clone(),
            status,
            reason: reason.map(str::to_owned),
            updated_at_ms: Some(current_unix_time_ms()),
        };
        if let Err(error) = session.append_control(ControlEntry::AgentResultContinuation(entry)) {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "agent result continuation status append failed: {error:#}"
            )));
            return;
        }
    }
}

pub(in crate::runner) fn read_conversation_queue_entries(
    session_log_path: &Path,
    current_session: &Option<Session>,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    if let Some(session) = current_session.as_ref() {
        return Ok(session.entries().to_vec());
    }
    JsonlSessionStore::read_entries(session_log_path)
        .map_err(|error| format!("failed to read conversation queue state: {error:#}"))
}

pub(in crate::runner) fn next_conversation_queue_id(
    entries: &[SessionLogEntry],
) -> std::result::Result<ConversationInputQueueId, String> {
    let existing = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ConversationInputQueued(queued)) => {
                Some(queued.queue_id.as_str())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for index in 1..=existing.len().saturating_add(1024) {
        let candidate = format!("queue_{index}");
        if !existing.contains(candidate.as_str()) {
            return ConversationInputQueueId::new(candidate)
                .map_err(|error| format!("failed to allocate queue id: {error:#}"));
        }
    }
    Err("failed to allocate queue id".to_owned())
}

pub(in crate::runner) fn conversation_prompt_hash(prompt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prompt.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn durable_conversation_prompt_hash(raw_prompt: &str, safe_prompt: &str) -> String {
    let safe_hash = conversation_prompt_hash(safe_prompt);
    if raw_prompt == safe_prompt {
        format!("safe:{safe_hash}")
    } else {
        format!("{EXACT_PROMPT_REQUIRED_HASH_PREFIX}{safe_hash}")
    }
}

pub(in crate::runner) fn queue_status_label(status: ConversationInputStatus) -> &'static str {
    match status {
        ConversationInputStatus::Queued => "queued",
        ConversationInputStatus::Dispatching => "dispatching",
        ConversationInputStatus::Delivered => "delivered",
        ConversationInputStatus::Rejected => "rejected",
        ConversationInputStatus::Cancelled => "cancelled",
        ConversationInputStatus::Stale => "stale",
        ConversationInputStatus::Unknown => "unknown",
    }
}

pub(in crate::runner) fn send_conversation_queue_update(
    message_tx: &mpsc::Sender<WorkerMessage>,
    entries: &[SessionLogEntry],
) {
    let projection = sigil_kernel::ConversationQueueProjection::from_entries(entries);
    let _ = message_tx.send(WorkerMessage::ConversationQueueUpdated {
        items: projection.items,
        paused: projection.paused,
        entries: entries.to_vec(),
    });
}

pub(in crate::runner) fn mark_stale_dispatching_conversation_queue_items(
    session: &mut Session,
    exact_prompts: &ExactConversationPromptStore,
    message_tx: &mpsc::Sender<WorkerMessage>,
) {
    let stale_queue_items = session
        .conversation_queue_projection()
        .items
        .into_iter()
        .filter_map(|item| {
            let missing_exact = item.status == ConversationInputStatus::Queued
                && item
                    .queued
                    .prompt_hash
                    .starts_with(EXACT_PROMPT_REQUIRED_HASH_PREFIX)
                && !exact_prompts.contains_key(&item.queued.queue_id);
            (item.status == ConversationInputStatus::Dispatching || missing_exact)
                .then_some((item.queued.queue_id, missing_exact))
        })
        .collect::<Vec<_>>();
    if stale_queue_items.is_empty() {
        return;
    }

    let mut changed = false;
    for (queue_id, missing_exact) in stale_queue_items {
        let status = ConversationInputStatusEntry {
            queue_id,
            status: ConversationInputStatus::Stale,
            reason: Some(if missing_exact {
                "exact sensitive follow-up was lost after restart".to_owned()
            } else {
                "stale after session restore without active run".to_owned()
            }),
            updated_at_ms: Some(current_unix_time_ms()),
        };
        if let Err(error) =
            session.append_control(ControlEntry::ConversationInputStatusChanged(status))
        {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "conversation queue restore skipped: {error:#}"
            )));
            break;
        }
        changed = true;
    }

    if changed {
        send_conversation_queue_update(message_tx, session.entries());
    }
}

pub(in crate::runner) fn append_queue_status_and_notify(
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    queue_id: ConversationInputQueueId,
    status: ConversationInputStatus,
    reason: Option<String>,
) {
    let Some(session) = current_session.as_mut() else {
        let _ = message_tx.send(WorkerMessage::Notice(
            "conversation queue status skipped: session state unavailable".to_owned(),
        ));
        return;
    };
    let entry = ConversationInputStatusEntry {
        queue_id,
        status,
        reason,
        updated_at_ms: Some(current_unix_time_ms()),
    };
    if let Err(error) = session.append_control(ControlEntry::ConversationInputStatusChanged(entry))
    {
        let _ = message_tx.send(WorkerMessage::Notice(format!(
            "conversation queue status append failed: {error:#}"
        )));
        return;
    }
    send_conversation_queue_update(message_tx, session.entries());
}

pub(in crate::runner) fn append_queue_failure_and_pause_and_notify(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    queue_id: ConversationInputQueueId,
    reason: String,
) {
    let controls = vec![
        ControlEntry::ConversationInputStatusChanged(ConversationInputStatusEntry {
            queue_id,
            status: ConversationInputStatus::Rejected,
            reason: Some(reason),
            updated_at_ms: Some(current_unix_time_ms()),
        }),
        ControlEntry::ConversationInputQueueControl(ConversationInputQueueControlEntry {
            action: ConversationInputQueueControlAction::Pause,
            reason: Some("queued run failed".to_owned()),
            updated_at_ms: Some(current_unix_time_ms()),
        }),
    ];
    match append_conversation_queue_control_entries(session_log_path, current_session, controls) {
        Ok(entries) => send_conversation_queue_update(message_tx, &entries),
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "conversation queue failure handling skipped: {error}"
            )));
        }
    }
}

pub(in crate::runner) fn mark_next_conversation_queue_item_dispatching(
    current_session: &mut Option<Session>,
    exact_prompts: &mut ExactConversationPromptStore,
    message_tx: &mpsc::Sender<WorkerMessage>,
) -> Option<ConversationInputQueuedEntry> {
    let session = current_session.as_mut()?;
    let projection = session.conversation_queue_projection();
    let queue_id = projection.next_dispatchable?;
    let mut queued = projection
        .items
        .iter()
        .find(|item| item.queued.queue_id == queue_id)
        .map(|item| item.queued.clone())?;
    if let Some(exact_prompt) = exact_prompts.remove(&queue_id) {
        queued.prompt = exact_prompt.expose_secret().to_owned();
    } else if queued
        .prompt_hash
        .starts_with(EXACT_PROMPT_REQUIRED_HASH_PREFIX)
    {
        let status = ConversationInputStatusEntry {
            queue_id,
            status: ConversationInputStatus::Stale,
            reason: Some("exact sensitive follow-up was lost after restart".to_owned()),
            updated_at_ms: Some(current_unix_time_ms()),
        };
        if let Err(error) =
            session.append_control(ControlEntry::ConversationInputStatusChanged(status))
        {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "conversation queue stale transition failed: {error:#}"
            )));
            return None;
        }
        send_conversation_queue_update(message_tx, session.entries());
        return None;
    }
    let status = ConversationInputStatusEntry {
        queue_id,
        status: ConversationInputStatus::Dispatching,
        reason: Some("dispatching".to_owned()),
        updated_at_ms: Some(current_unix_time_ms()),
    };
    if let Err(error) = session.append_control(ControlEntry::ConversationInputStatusChanged(status))
    {
        let _ = message_tx.send(WorkerMessage::Notice(format!(
            "conversation queue dispatch skipped: {error:#}"
        )));
        return None;
    }
    send_conversation_queue_update(message_tx, session.entries());
    Some(queued)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW_PROMPT: &str =
        "inspect https://example.com/private?signature=queue-secret-value exactly";

    #[test]
    fn sensitive_queue_prompt_is_safe_at_rest_but_exact_at_same_process_dispatch() {
        let mut session = Some(Session::new("test", "model"));
        let mut exact_prompts = ExactConversationPromptStore::new();

        let entries = queue_conversation_input(
            Path::new("unused.jsonl"),
            &mut session,
            &mut exact_prompts,
            RAW_PROMPT.to_owned(),
            ConversationInputKind::Chat,
            ConversationInputTarget::MainThread,
            ReasoningEffort::High,
        )
        .expect("sensitive follow-up should queue");

        let durable_json = serde_json::to_string(&entries).expect("entries should serialize");
        assert!(!durable_json.contains("queue-secret-value"));
        assert!(!durable_json.contains(RAW_PROMPT));
        assert!(durable_json.contains(EXACT_PROMPT_REQUIRED_HASH_PREFIX));

        let (message_tx, _message_rx) = mpsc::channel();
        let dispatched = mark_next_conversation_queue_item_dispatching(
            &mut session,
            &mut exact_prompts,
            &message_tx,
        )
        .expect("same-process dispatch should retain exact material");
        assert_eq!(dispatched.prompt, RAW_PROMPT);
        assert!(exact_prompts.is_empty());

        let durable_json = serde_json::to_string(
            session
                .as_ref()
                .expect("session should remain available")
                .entries(),
        )
        .expect("entries should serialize");
        assert!(!durable_json.contains("queue-secret-value"));
        assert!(!durable_json.contains(RAW_PROMPT));
    }

    #[test]
    fn sensitive_queue_prompt_without_process_local_exact_material_becomes_stale() {
        let mut original = Some(Session::new("test", "model"));
        let mut exact_prompts = ExactConversationPromptStore::new();
        let entries = queue_conversation_input(
            Path::new("unused.jsonl"),
            &mut original,
            &mut exact_prompts,
            RAW_PROMPT.to_owned(),
            ConversationInputKind::Chat,
            ConversationInputTarget::MainThread,
            ReasoningEffort::High,
        )
        .expect("sensitive follow-up should queue");

        let mut restored = Some(Session::from_entries("test", "model", entries));
        let mut restored_exact_prompts = ExactConversationPromptStore::new();
        let (message_tx, _message_rx) = mpsc::channel();
        let dispatched = mark_next_conversation_queue_item_dispatching(
            &mut restored,
            &mut restored_exact_prompts,
            &message_tx,
        );

        assert!(dispatched.is_none());
        let restored = restored.expect("session should remain available");
        assert!(restored.entries().iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
                if status.status == ConversationInputStatus::Stale
                    && status.reason.as_deref().is_some_and(|reason| reason.contains("lost after restart"))
        )));
        let durable_json =
            serde_json::to_string(restored.entries()).expect("entries should serialize");
        assert!(!durable_json.contains("queue-secret-value"));
        assert!(!durable_json.contains(RAW_PROMPT));
    }
}
