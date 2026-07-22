use crate::{
    HTTP_CONVERSATION_QUEUE_SCHEMA_VERSION, HttpConversationQueueCommandActionKind,
    HttpConversationQueueCommandReceipt, HttpConversationQueueGeneration,
    HttpConversationQueueItem, HttpConversationQueueItemKind, HttpConversationQueueItemStatus,
    HttpConversationQueuePromptMaterial, HttpConversationQueueView, HttpPermissionMode,
    HttpRunSnapshot, HttpRunStartCommandReceipt, HttpRunStatus,
    command_store::{
        HTTP_DURABLE_COMMAND_PROMPT_OMISSION, HttpDurableCommandStore, HttpStoredCommandClaim,
        HttpStoredCommandCompletion, HttpStoredCommandIdentity, HttpStoredCommandKey,
    },
};

fn identity(command_id: &str, fingerprint: char) -> HttpStoredCommandIdentity {
    HttpStoredCommandIdentity {
        key: HttpStoredCommandKey {
            session_id: "session-1".to_owned(),
            client_id: "client-1".to_owned(),
            command_id: command_id.to_owned(),
        },
        kind: "start".to_owned(),
        fingerprint_sha256: fingerprint.to_string().repeat(64),
    }
}

fn queue_identity(command_id: &str, fingerprint: char) -> HttpStoredCommandIdentity {
    let mut identity = identity(command_id, fingerprint);
    identity.kind = "queue".to_owned();
    identity
}

fn receipt(command_id: &str) -> HttpRunStartCommandReceipt {
    HttpRunStartCommandReceipt {
        command_id: command_id.to_owned(),
        client_id: "client-1".to_owned(),
        session_id: "session-1".to_owned(),
        correlation_id: None,
        run: HttpRunSnapshot {
            id: "run-1".to_owned(),
            session_id: "session-1".to_owned(),
            status: HttpRunStatus::Running,
            permission_mode: HttpPermissionMode::ReadOnly,
            reasoning_effort: None,
            prompt_preview: HTTP_DURABLE_COMMAND_PROMPT_OMISSION.to_owned(),
            pending_approval_call_ids: Vec::new(),
            stream_sequence: 1,
        },
        foreground_owner: None,
        replayed: false,
    }
}

fn queue_receipt(
    command_id: &str,
    prompt_preview: impl Into<String>,
) -> HttpConversationQueueCommandReceipt {
    let generation = HttpConversationQueueGeneration("7:event-queue-7".to_owned());
    HttpConversationQueueCommandReceipt {
        command_id: command_id.to_owned(),
        client_id: "client-1".to_owned(),
        session_id: "session-1".to_owned(),
        action: HttpConversationQueueCommandActionKind::Enqueue,
        expected_generation: HttpConversationQueueGeneration("6:event-queue-6".to_owned()),
        generation: generation.clone(),
        interrupt_owner: None,
        queue: HttpConversationQueueView {
            schema_version: HTTP_CONVERSATION_QUEUE_SCHEMA_VERSION,
            session_id: "session-1".to_owned(),
            generation,
            paused: false,
            total_items: 1,
            items: vec![HttpConversationQueueItem {
                entry_id: "queue-1".to_owned(),
                order: 0,
                kind: HttpConversationQueueItemKind::Chat,
                status: HttpConversationQueueItemStatus::Queued,
                prompt_preview: prompt_preview.into(),
                prompt_preview_truncated: false,
                prompt_material: HttpConversationQueuePromptMaterial::AvailableProcessLocal,
                dispatchable: true,
                blocked_reason: None,
                created_at_ms: Some(1),
                updated_at_ms: None,
            }],
            truncated: false,
            next_dispatchable_entry_id: Some("queue-1".to_owned()),
        },
        correlation_id: Some("correlation-1".to_owned()),
        replayed: false,
    }
}

#[test]
fn durable_command_store_replays_success_and_rejects_conflicting_identity() {
    let temp = tempfile::tempdir().expect("temp directory should create");
    let path = temp.path().join("commands.json");
    let stored = identity("command-1", 'a');
    {
        let store = HttpDurableCommandStore::open(&path, 8).expect("store should open");
        assert!(matches!(
            store.reserve(stored.clone()),
            Ok(HttpStoredCommandClaim::Execute)
        ));
        store
            .complete(
                &stored,
                HttpStoredCommandCompletion::Start(receipt("command-1")),
            )
            .expect("completion should persist");
    }

    let store = HttpDurableCommandStore::open(&path, 8).expect("store should reopen");
    assert!(matches!(
        store.reserve(stored),
        Ok(HttpStoredCommandClaim::Existing(completion))
            if matches!(*completion, HttpStoredCommandCompletion::Start(_))
    ));
    assert!(matches!(
        store.reserve(identity("command-1", 'b')),
        Ok(HttpStoredCommandClaim::Conflict)
    ));
}

#[test]
fn durable_command_store_seals_incomplete_reservation_and_never_evicts_at_capacity() {
    let temp = tempfile::tempdir().expect("temp directory should create");
    let path = temp.path().join("commands.json");
    let first = identity("command-1", 'a');
    {
        let store = HttpDurableCommandStore::open(&path, 1).expect("store should open");
        assert!(matches!(
            store.reserve(first.clone()),
            Ok(HttpStoredCommandClaim::Execute)
        ));
    }

    let store = HttpDurableCommandStore::open(&path, 1).expect("store should reopen");
    assert!(matches!(
        store.reserve(first),
        Ok(HttpStoredCommandClaim::Existing(completion))
            if *completion == HttpStoredCommandCompletion::Aborted
    ));
    assert!(matches!(
        store.reserve(identity("command-2", 'b')),
        Err(crate::HttpCommandStoreError::Saturated)
    ));
}

#[test]
fn durable_command_store_round_trips_queue_completion_without_prompt_material() {
    const EXACT_PROMPT: &str =
        "open https://example.com/private?token=queue-command-token-must-never-persist";

    let temp = tempfile::tempdir().expect("temp directory should create");
    let path = temp.path().join("commands.json");
    let stored = queue_identity("queue-command-1", 'c');
    let safe_prompt = sigil_kernel::project_conversation_prompt_for_persistence(EXACT_PROMPT);
    assert!(safe_prompt.exact_prompt_required);
    let expected = queue_receipt("queue-command-1", safe_prompt.safe_prompt);
    {
        let store = HttpDurableCommandStore::open(&path, 8).expect("store should open");
        assert!(matches!(
            store.reserve(stored.clone()),
            Ok(HttpStoredCommandClaim::Execute)
        ));
        store
            .complete(
                &stored,
                HttpStoredCommandCompletion::Queue(Box::new(expected.clone())),
            )
            .expect("queue completion should persist");

        let persisted = std::fs::read_to_string(&path).expect("command store should be readable");
        assert!(!persisted.contains("queue-command-token-must-never-persist"));
        assert!(persisted.contains("[redacted]"));
    }

    let store = HttpDurableCommandStore::open(&path, 8).expect("store should reopen");
    match store.reserve(stored) {
        Ok(HttpStoredCommandClaim::Existing(completion)) => {
            assert_eq!(
                *completion,
                HttpStoredCommandCompletion::Queue(Box::new(expected))
            );
        }
        _ => panic!("exact queue identity should replay its durable receipt"),
    }
    assert!(matches!(
        store.reserve(queue_identity("queue-command-1", 'd')),
        Ok(HttpStoredCommandClaim::Conflict)
    ));
}

#[test]
fn durable_command_store_validates_queue_receipt_session_before_writing() {
    let temp = tempfile::tempdir().expect("temp directory should create");
    let path = temp.path().join("commands.json");
    let stored = queue_identity("queue-command-2", 'e');
    let store = HttpDurableCommandStore::open(&path, 8).expect("store should open");
    assert!(matches!(
        store.reserve(stored.clone()),
        Ok(HttpStoredCommandClaim::Execute)
    ));
    let before = std::fs::read(&path).expect("reserved command should persist");

    let mut wrong_receipt_session = queue_receipt("queue-command-2", "safe prompt preview");
    wrong_receipt_session.session_id = "session-other".to_owned();
    assert!(matches!(
        store.complete(
            &stored,
            HttpStoredCommandCompletion::Queue(Box::new(wrong_receipt_session)),
        ),
        Err(crate::HttpCommandStoreError::Corrupt { .. })
    ));
    assert_eq!(
        std::fs::read(&path).expect("command store should remain readable"),
        before
    );

    let mut wrong_queue_session = queue_receipt("queue-command-2", "safe prompt preview");
    wrong_queue_session.queue.session_id = "session-other".to_owned();
    assert!(matches!(
        store.complete(
            &stored,
            HttpStoredCommandCompletion::Queue(Box::new(wrong_queue_session)),
        ),
        Err(crate::HttpCommandStoreError::Corrupt { .. })
    ));
    assert_eq!(
        std::fs::read(&path).expect("command store should remain readable"),
        before
    );

    store
        .complete(
            &stored,
            HttpStoredCommandCompletion::Queue(Box::new(queue_receipt(
                "queue-command-2",
                "safe prompt preview",
            ))),
        )
        .expect("matching queue receipt should persist after rejected mismatches");
}
