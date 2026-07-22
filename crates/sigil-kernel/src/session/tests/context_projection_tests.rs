use crate::{
    ConversationInputKind, ConversationInputQueueId, ConversationInputStatus,
    ConversationInputTarget, ToolCall, conversation_promotion_capability_digest,
    project_conversation_prompt_for_persistence,
};
use anyhow::Result;

use super::*;

fn started(attempt_id: &str) -> CompactionStartedEntry {
    CompactionStartedEntry {
        attempt_id: attempt_id.to_owned(),
        fallback_parent: CompactionFallbackParent::Root,
        initiation: CompactionInitiation::Manual,
        base_projection_revision: "projection-r1".to_owned(),
        started_at_unix_ms: 1,
    }
}

fn task_memory() -> TaskMemoryV1 {
    TaskMemoryV1 {
        memory_id: "memory-1".to_owned(),
        branch_id: None,
        valid_for_snapshot: "snapshot-1".to_owned(),
        supersedes: None,
        source_event_ids: vec!["event-source".to_owned()],
        objective: "Keep the compaction contract durable".to_owned(),
        active_plan: None,
        constraints: Vec::new(),
        decisions: Vec::new(),
        files_changed: Vec::new(),
        commands_run: Vec::new(),
        verification_results: Vec::new(),
        failed_attempts: Vec::new(),
        risks: Vec::new(),
        unresolved_issues: Vec::new(),
    }
}

fn applied(start: &StoredEvent) -> CompactionAppliedV2 {
    CompactionAppliedV2 {
        compaction_id: "compaction-1".to_owned(),
        attempt_id: "attempt-1".to_owned(),
        parent_compaction_id: None,
        branch_id: None,
        valid_for_snapshot: Some("snapshot-1".to_owned()),
        task_memory_id: Some("memory-1".to_owned()),
        checkpoint: ContinuationCheckpointV1::bound_to("memory-1", "snapshot-1"),
        base_projection_revision: "projection-r1".to_owned(),
        folded_through: CompactionCursor {
            session_id: start.session_id.clone(),
            through_stream_sequence: start.stream_sequence,
            through_event_id: start.event_id.clone(),
        },
        applied_at_unix_ms: 2,
    }
}

#[test]
fn v2_context_projection_preserves_raw_messages_until_applied_then_uses_v2_boundary() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    session.append_user_message(ModelMessage::user("first"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("second".to_owned()),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user("third"))?;

    let raw = session.context_projection();
    assert_eq!(raw.active_compaction_id, None);
    assert_eq!(raw.model_messages().len(), 3);

    let start = store.append_compaction_started(started("attempt-1"))?;
    store.append_task_memory_recorded_v1(
        "attempt-1",
        TaskMemoryRecordedV1::new(
            CompactionCursor {
                session_id: start.session_id.clone(),
                through_stream_sequence: start.stream_sequence,
                through_event_id: start.event_id.clone(),
            },
            task_memory(),
        )?,
    )?;

    let before_applied_bytes = std::fs::read(store.path())?;
    let before_applied = session
        .try_context_projection_from_durable()?
        .expect("store-backed session has a durable projection");
    assert_eq!(before_applied.active_compaction_id, None);
    assert_eq!(
        before_applied
            .model_messages()
            .iter()
            .map(|message| message.id.as_str())
            .collect::<Vec<_>>(),
        raw.model_messages()
            .iter()
            .map(|message| message.id.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(std::fs::read(store.path())?, before_applied_bytes);

    store.append_compaction_applied_v2(applied(&start))?;
    let activated = session
        .try_context_projection_from_durable()?
        .expect("store-backed session has a durable projection");
    assert_eq!(
        activated.active_compaction_id.as_deref(),
        Some("compaction-1")
    );
    assert_eq!(
        activated
            .folded_through
            .as_ref()
            .map(|cursor| cursor.through_event_id.as_str()),
        Some(start.event_id.as_str())
    );
    assert_eq!(
        activated
            .task_memory
            .as_ref()
            .map(|memory| memory.memory_id.as_str()),
        Some("memory-1")
    );
    assert!(matches!(
        activated.task_memory_snapshot_relation,
        Some(TaskMemorySnapshotRelation::CurrentUnknown)
    ));
    assert_eq!(
        activated
            .model_messages()
            .iter()
            .map(|message| message.content.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("first"), Some("second"), Some("third")]
    );

    let request = session.build_request(
        temp.path(),
        &crate::MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
        None,
    )?;
    let request_contents = request
        .messages
        .iter()
        .map(|message| message.content.as_deref())
        .collect::<Vec<_>>();
    assert!(request_contents.ends_with(&[Some("first"), Some("second"), Some("third")]));
    Ok(())
}

#[test]
fn promoted_user_is_live_immediately_but_durable_context_waits_for_delivery() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session-promoted.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    let queue_id = ConversationInputQueueId::new("context-promoted")?;
    let prompt = project_conversation_prompt_for_persistence("promoted context request");
    session.append_control(ControlEntry::ConversationInputQueued(
        ConversationInputQueuedEntry {
            queue_id: queue_id.clone(),
            target: ConversationInputTarget::MainThread,
            kind: ConversationInputKind::Chat,
            prompt_hash: prompt.prompt_hash.clone(),
            prompt: prompt.safe_prompt.clone(),
            reasoning_effort: None,
            created_at_ms: Some(1),
        },
    ))?;
    let revision = session
        .try_conversation_queue_durable_projection_from_durable()?
        .expect("durable queue projection")
        .revision
        .expect("queued event advances revision");
    let mut durable_user_message = ModelMessage::user(prompt.safe_prompt.clone());
    durable_user_message.id = "context-promoted-message".to_owned();
    let promotion = ConversationInputPromotedEntry {
        queue_id: queue_id.clone(),
        expected_queue_revision: revision,
        prompt_hash: prompt.prompt_hash,
        exact_prompt_required: false,
        durable_user_message: durable_user_message.clone(),
        capability_descriptors: Vec::new(),
        capability_digest: conversation_promotion_capability_digest(&[])?,
        dispatch_run_id: "context-promoted-run".to_owned(),
        promoted_at_ms: 2,
    };
    store.append_conversation_input_promoted(promotion.clone())?;

    let dispatching = session
        .try_context_projection_from_durable()?
        .expect("durable context projection");
    assert!(dispatching.model_messages().is_empty());
    session.record_durably_appended_conversation_input_promotion(promotion)?;
    assert_eq!(
        session
            .context_projection()
            .model_messages()
            .iter()
            .filter(|message| message.id == durable_user_message.id)
            .count(),
        1
    );

    store.append(&SessionLogEntry::Control(
        ControlEntry::ConversationInputStatusChanged(ConversationInputStatusEntry {
            queue_id,
            status: ConversationInputStatus::Delivered,
            reason: None,
            updated_at_ms: Some(3),
        }),
    ))?;
    let delivered = session
        .try_context_projection_from_durable()?
        .expect("durable context projection");
    assert_eq!(
        delivered
            .model_messages()
            .iter()
            .filter(|message| {
                message.id == durable_user_message.id
                    && message.content == durable_user_message.content
            })
            .count(),
        1
    );
    assert_eq!(
        JsonlSessionStore::read_event_records(store.path())?
            .iter()
            .filter(|record| {
                record.stored_event().event_kind() == Some(DurableEventType::UserMessageRecorded)
            })
            .count(),
        0
    );
    Ok(())
}

#[test]
fn portable_fold_projection_keeps_protected_messages_before_the_fold_cursor() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    session.append_user_message(ModelMessage::user("safe old request"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("safe old response".to_owned()),
        Vec::new(),
    ))?;
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "missing-result".to_owned(),
            name: "read_file".to_owned(),
            args_json: r#"{"path":"src/lib.rs"}"#.to_owned(),
        }],
    ))?;
    session.append_user_message(ModelMessage::user("latest request"))?;

    let records = JsonlSessionStore::read_event_records(store.path())?;
    let plan = CompactionFoldPlan::from_records(&records, 1)?;
    let unsafe_tool_call = plan
        .protected_events
        .iter()
        .find(|protected| protected.reason == CompactionFoldProtectionReason::UnsafeToolPair)
        .expect("unpaired tool call must remain protected");
    let retained = portable_retained_raw_event_ids_for_plan(&records, &plan)?;

    assert!(retained.contains(&unsafe_tool_call.event.event_id));
    assert!(
        plan.folded_event_ids
            .iter()
            .all(|event_id| !retained.contains(event_id))
    );
    Ok(())
}

#[test]
fn invalidated_task_memory_no_longer_activates_the_v2_context_boundary() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    session.append_user_message(ModelMessage::user("first"))?;

    let start = store.append_compaction_started(started("attempt-1"))?;
    store.append_task_memory_recorded_v1(
        "attempt-1",
        TaskMemoryRecordedV1::new(
            CompactionCursor {
                session_id: start.session_id.clone(),
                through_stream_sequence: start.stream_sequence,
                through_event_id: start.event_id.clone(),
            },
            task_memory(),
        )?,
    )?;
    let applied = store.append_compaction_applied_v2(applied(&start))?;
    store.append_task_memory_invalidated(TaskMemoryInvalidatedEntry {
        task_memory_id: "memory-1".to_owned(),
        reason: TaskMemoryInvalidationReason::Explicit,
        invalidated_by_event_id: applied.event_id,
    })?;

    let projection = session
        .try_context_projection_from_durable()?
        .expect("store-backed session has a durable projection");
    assert_eq!(projection.active_compaction_id, None);
    assert_eq!(
        projection
            .model_messages()
            .iter()
            .map(|message| message.content.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("first")]
    );
    Ok(())
}

#[test]
fn applied_v2_without_task_memory_still_activates_its_durable_boundary() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    session.append_user_message(ModelMessage::user("first"))?;

    let start = store.append_compaction_started(started("attempt-1"))?;
    let mut applied_without_memory = applied(&start);
    applied_without_memory.compaction_id = "compaction-no-memory".to_owned();
    applied_without_memory.valid_for_snapshot = None;
    applied_without_memory.task_memory_id = None;
    applied_without_memory.checkpoint = ContinuationCheckpointV1::empty();
    store.append_compaction_applied_v2(applied_without_memory)?;

    let projection = session
        .try_context_projection_from_durable()?
        .expect("store-backed session has a durable projection");
    assert_eq!(
        projection.active_compaction_id.as_deref(),
        Some("compaction-no-memory")
    );
    assert!(projection.task_memory.is_none());
    assert!(projection.checkpoint.is_none());
    assert_eq!(
        projection.model_messages()[0].content.as_deref(),
        Some("first")
    );
    Ok(())
}

#[test]
fn build_request_rejects_an_unsupported_legacy_stream_before_memory_snapshot_write() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let legacy = serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("old")))?;
    std::fs::write(&path, format!("{legacy}\n"))?;
    let store = JsonlSessionStore::new(&path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    let before = std::fs::read(&path)?;

    let error = session
        .build_request(
            temp.path(),
            &crate::MemoryConfig { enabled: false },
            Vec::new(),
            None,
            None,
            None,
        )
        .expect_err("legacy stream must fail before request-side durable writes");

    assert!(format!("{error:#}").contains("unsupported legacy SessionLogEntry format"));
    assert_eq!(std::fs::read(&path)?, before);
    assert!(session.entries().is_empty());
    Ok(())
}

#[test]
fn pre_turn_candidate_request_keeps_exact_transient_input_out_of_session_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    session.append_user_message(ModelMessage::user("previous durable turn"))?;
    let before_stream = std::fs::read(store.path())?;
    let before_entries = session.entries().to_vec();

    let request = session.build_pre_turn_candidate_request(
        temp.path(),
        &crate::MemoryConfig { enabled: false },
        Vec::new(),
        Some(1024),
        None,
        None,
        None,
        &[ModelMessage::user("queued exact pre-turn input")],
        RuntimeContextCandidates::default(),
        &[],
    )?;

    assert!(
        request
            .messages
            .iter()
            .any(|message| message.content.as_deref() == Some("queued exact pre-turn input"))
    );
    assert_eq!(std::fs::read(store.path())?, before_stream);
    assert_eq!(session.entries().len(), before_entries.len());
    assert!(session.entries().iter().all(|entry| !matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(_))
    )));
    Ok(())
}

#[test]
fn task_memory_snapshot_relation_is_metadata_only() {
    let projection = SessionContextProjection {
        projection_schema_version: SESSION_CONTEXT_PROJECTION_SCHEMA_VERSION,
        active_compaction_id: Some("compaction-1".to_owned()),
        folded_through: None,
        task_memory: Some(task_memory()),
        task_memory_snapshot_relation: Some(TaskMemorySnapshotRelation::CurrentUnknown),
        checkpoint: None,
        retained_entries: vec![SessionProjectionEntry {
            message: ModelMessage::user("first"),
        }],
        trust_projection: ContextTrustProjection::default(),
    }
    .with_current_workspace_snapshot(Some("snapshot-2".to_owned()));

    assert!(matches!(
        projection.task_memory_snapshot_relation,
        Some(TaskMemorySnapshotRelation::Changed { ref captured, ref current })
            if captured == "snapshot-1" && current == "snapshot-2"
    ));
    assert_eq!(
        projection.model_messages()[0].content.as_deref(),
        Some("first")
    );
}
