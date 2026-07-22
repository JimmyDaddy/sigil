use crate::{
    HttpPermissionMode, HttpRunSnapshot, HttpRunStartCommandReceipt, HttpRunStatus,
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
