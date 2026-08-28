use super::*;
use sigil_application::{
    APPLICATION_CONTRACT_SCHEMA_VERSION, ApplicationFrontier, ApplicationInstanceId,
    AuthenticatedSubject, SessionScopeId, WorkspaceScopeId,
};
use sigil_kernel::resource::{AuthorityGeneration, CanonicalHash};
use sigil_resource_authority::storage::{
    AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
};

fn scope() -> ApplicationScope {
    ApplicationScope {
        application_instance: ApplicationInstanceId::new("application").expect("instance"),
        authenticated_subject: AuthenticatedSubject::new("subject").expect("subject"),
        workspace: Some(WorkspaceScopeId::new("workspace").expect("workspace")),
        session: Some(SessionScopeId::new("session").expect("session")),
    }
}

fn acknowledgement(event_id: &str, through_sequence: u64) -> ProjectionDeliveryAck {
    let scope = scope();
    ProjectionDeliveryAck {
        scope: scope.clone(),
        observer_generation: 3,
        event_id: event_id.to_owned(),
        frontier: ApplicationFrontier {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope,
            writer_generation: 1,
            stream_generation: 1,
            through_sequence,
            durable_cursor: format!("cursor-{through_sequence}"),
        },
    }
}

fn journal_bytes(acknowledgements: &[ProjectionDeliveryAck]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for acknowledgement in acknowledgements {
        let entry = DurableDeliveryAckJournalEntry {
            schema_version: APPLICATION_DELIVERY_ACK_SCHEMA_VERSION,
            acknowledgement: acknowledgement.clone(),
        };
        bytes.extend(serde_json::to_vec(&entry).expect("journal entry"));
        bytes.push(b'\n');
    }
    bytes
}

#[test]
fn ack_journal_replays_exact_duplicates() {
    let first = acknowledgement("event-1", 1);
    let bytes = journal_bytes(&[first.clone(), first.clone()]);
    let entries = decode_entries(&bytes, &scope(), 3).expect("replay");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries.get("event-1"), Some(&first));
}

#[test]
fn ack_journal_rejects_rewritten_event_identity() {
    let bytes = journal_bytes(&[acknowledgement("event-1", 1), acknowledgement("event-1", 2)]);
    let error = decode_entries(&bytes, &scope(), 3).expect_err("rewritten event");
    assert!(matches!(error, ApplicationError::CorruptProjection(_)));
}

#[test]
fn ack_journal_rejects_partial_record_and_wrong_observer() {
    let first = acknowledgement("event-1", 1);
    let partial = journal_bytes(std::slice::from_ref(&first));
    let error =
        decode_entries(&partial[..partial.len() - 1], &scope(), 3).expect_err("partial record");
    assert!(matches!(error, ApplicationError::CorruptProjection(_)));

    let error = decode_entries(&journal_bytes(&[first]), &scope(), 4).expect_err("wrong observer");
    assert!(matches!(error, ApplicationError::ScopeMismatch));
}

#[test]
fn ack_store_writes_through_the_managed_writer() {
    let directory = tempfile::tempdir().expect("directory");
    let authority_generation = AuthorityGeneration {
        epoch: 1,
        instance_hash: CanonicalHash::from_bytes([0x28; 32]),
    };
    let cutover_manifest_hash = CanonicalHash::from_bytes([0x2a; 32]);
    let mut table = AuthorityStorageGrantTableV1::new();
    table
        .register(
            crate::managed_storage_writer::grant_for_channel_with_context(
                StorageWriterChannelV1::ApplicationControlLog,
                0x31,
                authority_generation,
                cutover_manifest_hash,
            ),
        )
        .expect("register ACK grant");
    let service = Arc::new(AuthorityManagedStorageServiceV1::new(
        table,
        authority_generation,
    ));
    let writer = Arc::new(ManagedStorageWriterAdapterV1::new(
        service,
        directory.path().to_path_buf(),
        cutover_manifest_hash,
    ));
    let expected_scope = scope();
    let acknowledgement = acknowledgement("event-1", 1);
    let store = RuntimeApplicationDeliveryAckStore::open(
        Arc::clone(&writer),
        "ack-roundtrip",
        expected_scope.clone(),
        3,
    )
    .expect("open ACK store");
    futures::executor::block_on(
        <RuntimeApplicationDeliveryAckStore as crate::RuntimeApplicationDeliveryAcker>::acknowledge(
            &store,
            acknowledgement.clone(),
        ),
    )
    .expect("durable ACK");
    assert!(store.contains(&acknowledgement));
    drop(store);

    let record_path = writer
        .managed_named_leaf_path(
            StorageWriterChannelV1::ApplicationControlLog,
            "ack-roundtrip",
        )
        .expect("managed ACK path")
        .join("records.jsonl");
    let bytes = std::fs::read(record_path).expect("managed ACK journal");
    let entries = decode_entries(&bytes, &expected_scope, 3).expect("replay written ACK");
    assert_eq!(entries.get("event-1"), Some(&acknowledgement));
}
