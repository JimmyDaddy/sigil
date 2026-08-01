use crate::{
    HTTP_CONVERSATION_QUEUE_SCHEMA_VERSION, HttpConversationQueueCommandActionKind,
    HttpConversationQueueCommandReceipt, HttpConversationQueueGeneration,
    HttpConversationQueueItem, HttpConversationQueueItemKind, HttpConversationQueueItemStatus,
    HttpConversationQueuePromptMaterial, HttpConversationQueueView,
    HttpConversationRecoveryCommandActionKind, HttpConversationRecoveryCommandReceipt,
    HttpConversationRecoveryView, HttpIntentDropCommandReceipt, HttpPermissionMode,
    HttpRunSnapshot, HttpRunStartCommandReceipt, HttpRunStatus,
    HttpTaskIntegrationAcceptanceCommandReceipt, HttpTaskIntegrationAcceptanceView,
    HttpTerminalLifecycleView, HttpTerminalTaskCancelCommandReceipt,
    command_store::{
        HTTP_DURABLE_COMMAND_PROMPT_OMISSION, HttpDurableCommandStore, HttpStoredCommandClaim,
        HttpStoredCommandCompletion, HttpStoredCommandIdentity, HttpStoredCommandKey,
    },
};
use sigil_kernel::{
    IntegrationPlanId, IntegrationPromotionStatus, TaskId, TaskIntegrationReviewRequest,
    TerminalReadinessStatus, TerminalTaskStatus, VerificationVerdict,
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

fn recovery_identity(command_id: &str, fingerprint: char) -> HttpStoredCommandIdentity {
    let mut identity = identity(command_id, fingerprint);
    identity.kind = "recovery".to_owned();
    identity
}

fn integration_identity(command_id: &str, fingerprint: char) -> HttpStoredCommandIdentity {
    let mut identity = identity(command_id, fingerprint);
    identity.kind = "integration".to_owned();
    identity
}

fn intent_drop_identity(command_id: &str, fingerprint: char) -> HttpStoredCommandIdentity {
    let mut identity = identity(command_id, fingerprint);
    identity.kind = "intent_drop".to_owned();
    identity
}

fn terminal_cancel_identity(command_id: &str, fingerprint: char) -> HttpStoredCommandIdentity {
    let mut identity = identity(command_id, fingerprint);
    identity.kind = "terminal_cancel".to_owned();
    identity
}

fn terminal_cancel_receipt(command_id: &str) -> HttpTerminalTaskCancelCommandReceipt {
    HttpTerminalTaskCancelCommandReceipt {
        command_id: command_id.to_owned(),
        client_id: "client-1".to_owned(),
        session_id: "session-1".to_owned(),
        expected_stream_sequence: Some(3),
        correlation_id: Some("correlation-1".to_owned()),
        run_id: "run-1".to_owned(),
        terminal_task: HttpTerminalLifecycleView {
            task_id: "terminal-1".to_owned(),
            execution_backend: None,
            sandbox_profile: None,
            generation: 4,
            status: TerminalTaskStatus::Cancelled,
            readiness: TerminalReadinessStatus::None,
            total_output_bytes: 16,
            emitted_at_ms: 20,
        },
        replayed: false,
    }
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
            pending_approvals: Vec::new(),
            approval_lifecycles: Vec::new(),
            terminal_tasks: Vec::new(),
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

fn recovery_receipt(command_id: &str) -> HttpConversationRecoveryCommandReceipt {
    HttpConversationRecoveryCommandReceipt {
        command_id: command_id.to_owned(),
        client_id: "client-1".to_owned(),
        session_id: "session-1".to_owned(),
        action: HttpConversationRecoveryCommandActionKind::ForkConversation,
        compaction: None,
        compaction_review: None,
        tool_output_shrink: None,
        restore: None,
        fork: None,
        recovery: HttpConversationRecoveryView {
            checkpoints: Vec::new(),
            fork_points: Vec::new(),
            through_stream_sequence: 9,
        },
        correlation_id: Some("correlation-1".to_owned()),
        replayed: false,
    }
}

fn integration_receipt(command_id: &str) -> HttpTaskIntegrationAcceptanceCommandReceipt {
    let request = TaskIntegrationReviewRequest {
        request_id: "integration-review-request".to_owned(),
        task_id: TaskId::new("task-1").expect("task id"),
        plan_id: IntegrationPlanId::new("plan-1").expect("plan id"),
        plan_version: 1,
        preview_digest: "sha256:preview".to_owned(),
    };
    HttpTaskIntegrationAcceptanceCommandReceipt {
        command_id: command_id.to_owned(),
        client_id: "client-1".to_owned(),
        session_id: "session-1".to_owned(),
        correlation_id: Some("correlation-1".to_owned()),
        acceptance: HttpTaskIntegrationAcceptanceView {
            request,
            promotion_status: IntegrationPromotionStatus::Promoted,
            parent_verdict: Some(VerificationVerdict::NotApplicable),
            can_continue: true,
            promotion_cleanup_error: None,
            parent_cleanup_error: None,
        },
        replayed: false,
    }
}

fn intent_drop_receipt(command_id: &str) -> HttpIntentDropCommandReceipt {
    serde_json::from_value(serde_json::json!({
        "command_id": command_id,
        "client_id": "client-1",
        "session_id": "session-1",
        "correlation_id": "correlation-1",
        "execution": {
            "preview": {
                "schema_version": 1,
                "operation_id": "intent-drop-telemetry",
                "operation_kind": "drop",
                "stack_id": "intent-stack-test",
                "stack_version": 1,
                "target_intents": [{
                    "intent_id": "telemetry",
                    "version": 1
                }],
                "target_is_leaf": true,
                "workspace_revision": 7,
                "file_effects": [{
                    "normalized_relative_path": "src/telemetry.rs",
                    "action": "update",
                    "artifact_ids": ["telemetry-artifact"]
                }],
                "retained_intents": [],
                "verification_impacts": [],
                "conflicts": [],
                "preview_digest": format!("sha256:jcs-v1:{}", "b".repeat(64))
            },
            "resolution": "committed",
            "mutation_batch_id": "intent-drop-batch",
            "committed_operation_ids": ["intent-drop-file-1"],
            "result_snapshot_id": "snapshot-after-drop",
            "error_code": null
        },
        "replayed": false
    }))
    .expect("Intent Drop receipt fixture should decode")
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
fn durable_command_store_round_trips_terminal_cancel_completion() {
    let temp = tempfile::tempdir().expect("temp directory should create");
    let path = temp.path().join("commands.json");
    let stored = terminal_cancel_identity("terminal-cancel-command-1", 'e');
    let expected = terminal_cancel_receipt("terminal-cancel-command-1");
    {
        let store = HttpDurableCommandStore::open(&path, 8).expect("store should open");
        assert!(matches!(
            store.reserve(stored.clone()),
            Ok(HttpStoredCommandClaim::Execute)
        ));
        store
            .complete(
                &stored,
                HttpStoredCommandCompletion::TerminalCancel(expected.clone()),
            )
            .expect("terminal cancellation completion should persist");
    }

    let store = HttpDurableCommandStore::open(&path, 8).expect("store should reopen");
    assert!(matches!(
        store.reserve(stored),
        Ok(HttpStoredCommandClaim::Existing(completion))
            if *completion == HttpStoredCommandCompletion::TerminalCancel(expected)
    ));
}

#[test]
fn durable_command_store_round_trips_recovery_completion() {
    let temp = tempfile::tempdir().expect("temp directory should create");
    let path = temp.path().join("commands.json");
    let stored = recovery_identity("recovery-command-1", 'f');
    let expected = recovery_receipt("recovery-command-1");
    {
        let store = HttpDurableCommandStore::open(&path, 8).expect("store should open");
        assert!(matches!(
            store.reserve(stored.clone()),
            Ok(HttpStoredCommandClaim::Execute)
        ));
        store
            .complete(
                &stored,
                HttpStoredCommandCompletion::Recovery(Box::new(expected.clone())),
            )
            .expect("recovery completion should persist");
    }

    let store = HttpDurableCommandStore::open(&path, 8).expect("store should reopen");
    assert!(matches!(
        store.reserve(stored),
        Ok(HttpStoredCommandClaim::Existing(completion))
            if *completion == HttpStoredCommandCompletion::Recovery(Box::new(expected))
    ));
    assert!(matches!(
        store.reserve(recovery_identity("recovery-command-1", 'a')),
        Ok(HttpStoredCommandClaim::Conflict)
    ));
}

#[test]
fn durable_command_store_round_trips_task_integration_completion() {
    let temp = tempfile::tempdir().expect("temp directory should create");
    let path = temp.path().join("commands.json");
    let stored = integration_identity("integration-command-1", '9');
    let expected = integration_receipt("integration-command-1");
    {
        let store = HttpDurableCommandStore::open(&path, 8).expect("store should open");
        assert!(matches!(
            store.reserve(stored.clone()),
            Ok(HttpStoredCommandClaim::Execute)
        ));
        store
            .complete(
                &stored,
                HttpStoredCommandCompletion::Integration(Box::new(expected.clone())),
            )
            .expect("integration completion should persist");
    }

    let store = HttpDurableCommandStore::open(&path, 8).expect("store should reopen");
    assert!(matches!(
        store.reserve(stored),
        Ok(HttpStoredCommandClaim::Existing(completion))
            if *completion == HttpStoredCommandCompletion::Integration(Box::new(expected))
    ));
    assert!(matches!(
        store.reserve(integration_identity("integration-command-1", '8')),
        Ok(HttpStoredCommandClaim::Conflict)
    ));
}

#[test]
fn durable_command_store_round_trips_intent_drop_completion() {
    let temp = tempfile::tempdir().expect("temp directory should create");
    let path = temp.path().join("commands.json");
    let stored = intent_drop_identity("intent-drop-command-1", '7');
    let expected = intent_drop_receipt("intent-drop-command-1");
    {
        let store = HttpDurableCommandStore::open(&path, 8).expect("store should open");
        assert!(matches!(
            store.reserve(stored.clone()),
            Ok(HttpStoredCommandClaim::Execute)
        ));
        store
            .complete(
                &stored,
                HttpStoredCommandCompletion::IntentDrop(Box::new(expected.clone())),
            )
            .expect("Intent Drop completion should persist");
    }

    let store = HttpDurableCommandStore::open(&path, 8).expect("store should reopen");
    assert!(matches!(
        store.reserve(stored),
        Ok(HttpStoredCommandClaim::Existing(completion))
            if *completion == HttpStoredCommandCompletion::IntentDrop(Box::new(expected))
    ));
    assert!(matches!(
        store.reserve(intent_drop_identity("intent-drop-command-1", '6')),
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
