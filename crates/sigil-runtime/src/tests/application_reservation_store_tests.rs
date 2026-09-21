use super::*;
use sigil_application::{
    ApplicationCommandId, ApplicationCommandReceipt, ApplicationDomainCommitRef,
    ApplicationDomainReceipt, ApplicationFrontier, ApplicationInstanceId, ApplicationScope,
    AuthenticatedSubject, CommandEffectBinding, CommandLifecyclePhase, CommandRecoveryBinding,
    EffectSettlementClass, WorkspaceScopeId,
};

fn key() -> CommandReservationKey {
    let principal = AuthenticatedSubject::new("subject").expect("subject");
    let scope = ApplicationScope {
        application_instance: ApplicationInstanceId::new("instance").expect("instance"),
        authenticated_subject: principal.clone(),
        workspace: Some(WorkspaceScopeId::new("workspace").expect("workspace")),
        session: None,
    };
    CommandReservationKey {
        application_instance: scope.application_instance.clone(),
        authority_scope: scope,
        principal,
        client_epoch: 1,
        command_id: ApplicationCommandId::new("command").expect("command"),
    }
}

fn fingerprint() -> String {
    "a".repeat(64)
}

fn effect_binding(key: &CommandReservationKey) -> CommandEffectBinding {
    CommandEffectBinding {
        command_id: key.command_id.clone(),
        command_kind: "test".to_owned(),
        reservation_fingerprint: fingerprint(),
        recovery: CommandRecoveryBinding {
            key: key.clone(),
            phase: CommandLifecyclePhase::EffectStarted,
        },
        owner_effect_id: "effect-1".to_owned(),
    }
}

fn domain_receipt(key: &CommandReservationKey) -> ApplicationDomainReceipt {
    ApplicationDomainReceipt {
        command_id: key.command_id.clone(),
        command_kind: "test".to_owned(),
        frontier: ApplicationFrontier {
            schema_version: sigil_application::APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope: key.authority_scope.clone(),
            writer_generation: 1,
            stream_generation: 1,
            through_sequence: 1,
            durable_cursor: "fixture-domain-1".to_owned(),
        },
        settlement: EffectSettlementClass::AtomicDurableMutation,
        summary: "fixture domain commit".to_owned(),
        domain_commit: ApplicationDomainCommitRef {
            source_session_scope_id: None,
            source_event_id: "fixture-domain-event-1".to_owned(),
            source_sequence: 1,
            source_digest: fingerprint(),
        },
        outcome: None,
    }
}

fn append_entry(bytes: &mut Vec<u8>, operation: DurableReservationOperation) {
    let entry = DurableReservationJournalEntry {
        schema_version: APPLICATION_RESERVATION_SCHEMA_VERSION,
        operation,
    };
    bytes.extend(serde_json::to_vec(&entry).expect("journal entry"));
    bytes.push(b'\n');
}

fn production_writer(root: &std::path::Path) -> Arc<ManagedStorageWriterAdapterV1> {
    use crate::managed_storage_writer::grant_for_channel_with_context;
    use sigil_kernel::{
        capability_issuer::KernelCapabilityBrokerV1,
        resource::{AuthorityGeneration, CanonicalHash},
    };
    use sigil_resource_authority::storage::{
        AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
    };
    let generation = AuthorityGeneration {
        epoch: 1,
        instance_hash: CanonicalHash::from_bytes([21; 32]),
    };
    let manifest = CanonicalHash::from_bytes([22; 32]);
    let mut grants = AuthorityStorageGrantTableV1::new();
    for (index, channel) in [
        StorageWriterChannelV1::ApplicationControlLog,
        StorageWriterChannelV1::ApplicationCommandIndex,
        StorageWriterChannelV1::ApplicationControlRecovery,
        StorageWriterChannelV1::InputHistory,
    ]
    .into_iter()
    .enumerate()
    {
        grants
            .register(grant_for_channel_with_context(
                channel,
                40 + index as u8,
                generation,
                manifest,
            ))
            .expect("current grant");
    }
    let service = Arc::new(
        AuthorityManagedStorageServiceV1::new_with_state_root(grants, generation, root)
            .expect("authority"),
    );
    Arc::new(ManagedStorageWriterAdapterV1::with_storage_issuer(
        service,
        root.to_owned(),
        manifest,
        Arc::new(KernelCapabilityBrokerV1::new()),
    ))
}

fn command_request(id: usize, padding: usize) -> ApplicationCommandRequest {
    use sigil_application::{
        ApplicationCommand, ApplicationCommandEnvelope, CommandAdmissionContext, ExpectedFrontier,
        HostConnectionInstanceId, McpCommand,
    };
    let key = key();
    ApplicationCommandRequest {
        admission: CommandAdmissionContext::host_bound(
            key.principal,
            1,
            HostConnectionInstanceId::new("test-connection").expect("connection"),
            key.authority_scope.clone(),
        )
        .expect("admission"),
        envelope: ApplicationCommandEnvelope {
            schema_version: sigil_application::APPLICATION_CONTRACT_SCHEMA_VERSION,
            command_id: ApplicationCommandId::new(format!("command-{id}")).expect("command id"),
            correlation_id: None,
            expected_frontier: ExpectedFrontier {
                scope: key.authority_scope,
                writer_generation: 1,
                through_sequence: 3,
            },
            command: ApplicationCommand::Mcp(McpCommand::Refresh {
                binding: "x".repeat(padding.max(1)),
            }),
        },
    }
}

#[test]
fn command_journal_crosses_old_history_bounds_and_rebuilds_corrupt_index() -> anyhow::Result<()> {
    let fixture = tempfile::tempdir()?;
    let writer = production_writer(fixture.path());
    let lease = writer.acquire_named(StorageWriterChannelV1::ApplicationControlLog, "history")?;
    let mut bytes = Vec::new();
    for id in 0..4200 {
        let request = command_request(id, 4096);
        let key = request
            .admission
            .reservation_key(&request.envelope.command_id);
        append_entry(
            &mut bytes,
            DurableReservationOperation::ReserveWithContextV1 {
                key,
                fingerprint: sigil_application::command_fingerprint(&request)?,
                request: Box::new(request),
            },
        );
    }
    assert!(bytes.len() > 16 * 1024 * 1024);
    writer.write_record(&lease, &bytes[..bytes.len() - 1])?;
    writer.finalize(lease)?;
    let store = ManagedApplicationReservationStore::open(Arc::clone(&writer), "history")?;
    let index_path = store
        .index_lease
        .lock()
        .expect("index")
        .as_ref()
        .expect("index lease")
        .path()
        .join("records.sqlite3");
    let next = command_request(4200, 1);
    let next_key = next.admission.reservation_key(&next.envelope.command_id);
    assert_eq!(
        futures::executor::block_on(store.reserve(
            next_key.clone(),
            sigil_application::command_fingerprint(&next)?,
            next.clone()
        ))?,
        RuntimeApplicationReservationAdmission::Reserved
    );
    drop(store);
    std::fs::write(&index_path, b"corrupt rebuildable index")?;
    let reopened = ManagedApplicationReservationStore::open(Arc::clone(&writer), "history")?;
    // RuntimeState ceilings are class-wide. A small, unrelated owner must remain
    // usable while the canonical journal and rebuildable index remain charged.
    let history = writer.acquire(StorageWriterChannelV1::InputHistory)?;
    writer.write_record(&history, b"independent input history")?;
    writer.finalize(history)?;
    assert_eq!(
        futures::executor::block_on(reopened.original_command_context(next_key))?
            .map(|context| context.expected_frontier),
        Some(next.envelope.expected_frontier)
    );
    let first = command_request(0, 4096);
    let admission = futures::executor::block_on(reopened.reserve(
        first.admission.reservation_key(&first.envelope.command_id),
        sigil_application::command_fingerprint(&first)?,
        first,
    ))?;
    assert!(matches!(
        admission,
        RuntimeApplicationReservationAdmission::Existing(_)
    ));
    Ok(())
}

#[test]
fn namespace_setup_failure_releases_holder_for_same_process_retry() -> anyhow::Result<()> {
    let fixture = tempfile::tempdir()?;
    let writer = production_writer(fixture.path());
    let path = writer.managed_leaf_path(StorageWriterChannelV1::InputHistory)?;
    std::fs::create_dir_all(path.parent().expect("channel parent"))?;
    std::fs::write(&path, b"injected directory creation failure")?;
    assert!(
        writer
            .acquire(StorageWriterChannelV1::InputHistory)
            .is_err()
    );
    std::fs::remove_file(&path)?;
    let lease = writer.acquire(StorageWriterChannelV1::InputHistory)?;
    writer.write_record(&lease, b"retry after setup failure")?;
    writer.finalize(lease)?;
    assert_eq!(
        std::fs::read(path.join("records.jsonl"))?,
        b"retry after setup failure\n"
    );
    Ok(())
}

#[test]
fn broken_command_tail_retains_prefix_and_blocks_new_effects() -> anyhow::Result<()> {
    let fixture = tempfile::tempdir()?;
    let writer = production_writer(fixture.path());
    let original = command_request(1, 1);
    let key = original
        .admission
        .reservation_key(&original.envelope.command_id);
    let store = ManagedApplicationReservationStore::open(Arc::clone(&writer), "broken")?;
    futures::executor::block_on(store.reserve(
        key.clone(),
        sigil_application::command_fingerprint(&original)?,
        original.clone(),
    ))?;
    let log = store
        .lease
        .lock()
        .expect("lease")
        .as_ref()
        .expect("journal")
        .path()
        .join("records.jsonl");
    drop(store);
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&log)?
        .write_all(b"{\"schema_version\":")?;
    let before = std::fs::read(&log)?;
    let reopened = ManagedApplicationReservationStore::open(Arc::clone(&writer), "broken")?;
    assert_eq!(
        futures::executor::block_on(reopened.original_command_context(key))?
            .map(|context| context.expected_frontier),
        Some(original.envelope.expected_frontier)
    );
    let next = command_request(2, 1);
    assert!(
        futures::executor::block_on(reopened.reserve(
            next.admission.reservation_key(&next.envelope.command_id),
            sigil_application::command_fingerprint(&next)?,
            next
        ))
        .is_err()
    );
    assert_eq!(std::fs::read(log)?, before);
    Ok(())
}

#[test]
fn command_index_with_valid_sqlite_but_wrong_schema_rebuilds_from_source() -> anyhow::Result<()> {
    let fixture = tempfile::tempdir()?;
    let writer = production_writer(fixture.path());
    let request = command_request(17, 1);
    let key = request
        .admission
        .reservation_key(&request.envelope.command_id);
    let store = ManagedApplicationReservationStore::open(Arc::clone(&writer), "wrong-schema")?;
    futures::executor::block_on(store.reserve(
        key.clone(),
        sigil_application::command_fingerprint(&request)?,
        request.clone(),
    ))?;
    let path = store
        .index_lease
        .lock()
        .expect("index")
        .as_ref()
        .expect("index lease")
        .path()
        .join("records.sqlite3");
    drop(store);
    let connection = rusqlite::Connection::open(&path)?;
    connection.execute_batch("DROP TABLE reservations; CREATE TABLE reservations (other TEXT);")?;
    drop(connection);
    let reopened = ManagedApplicationReservationStore::open(writer, "wrong-schema")?;
    assert_eq!(
        futures::executor::block_on(reopened.original_command_context(key))?
            .map(|value| value.expected_frontier),
        Some(request.envelope.expected_frontier)
    );
    Ok(())
}

#[test]
fn journal_replay_preserves_each_monotonic_lifecycle_phase() {
    let key = key();
    let fingerprint = fingerprint();
    let binding = effect_binding(&key);
    let domain = domain_receipt(&key);
    let mut bytes = Vec::new();
    append_entry(
        &mut bytes,
        DurableReservationOperation::Reserve {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
        },
    );
    append_entry(
        &mut bytes,
        DurableReservationOperation::DispatchStarted {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
        },
    );
    append_entry(
        &mut bytes,
        DurableReservationOperation::EffectStarted {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
            binding: Box::new(binding),
        },
    );
    append_entry(
        &mut bytes,
        DurableReservationOperation::DomainCommitted {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
            receipt: Box::new(domain.clone()),
        },
    );
    append_entry(
        &mut bytes,
        DurableReservationOperation::Settled {
            key: key.clone(),
            fingerprint,
            receipt: Box::new(ApplicationCommandReceipt::Settled(domain)),
        },
    );

    let entries = decode_entries(&bytes).expect("replay");
    let record = entries.get(&key).expect("record");
    assert!(matches!(record.state, DurableReservationState::Settled(_)));
}

#[test]
fn control_log_rotation_keeps_old_keys_and_activates_only_the_fixed_successor() -> anyhow::Result<()>
{
    let fixture = tempfile::tempdir()?;
    let writer = production_writer(fixture.path());
    let original = command_request(1, 1);
    let original_key = original
        .admission
        .reservation_key(&original.envelope.command_id);
    let fingerprint = sigil_application::command_fingerprint(&original)?;
    let store = ManagedApplicationReservationStore::open(Arc::clone(&writer), "rotate")?;
    futures::executor::block_on(store.reserve(
        original_key.clone(),
        fingerprint.clone(),
        original.clone(),
    ))?;
    let path = store
        .lease
        .lock()
        .expect("lease")
        .as_ref()
        .expect("journal")
        .path()
        .join("records.jsonl");
    drop(store);
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)?
        .write_all(b"{partial")?;
    let original_bytes = std::fs::read(&path)?;
    let store = ManagedApplicationReservationStore::open(Arc::clone(&writer), "rotate")?;
    let preview = store.preview_control_log_recovery()?;
    // RA can activate the exact successor before the rebuildable index becomes available.
    // Reopening the UI must still preview and resume this same operation.
    let index = writer.acquire_named(
        StorageWriterChannelV1::ApplicationCommandIndex,
        &super::recovery::generation_key("rotate", 1),
    )?;
    let unavailable_index = index.path().join("records.sqlite3");
    writer.finalize(index)?;
    std::fs::create_dir(&unavailable_index)?;
    assert!(store.seal_and_rotate_control_log(&preview).is_err());
    assert_eq!(store.preview_control_log_recovery()?, preview);
    std::fs::remove_dir(unavailable_index)?;
    let binding = store.seal_and_rotate_control_log(&preview)?;
    assert_eq!(binding.command_generation, 1);
    assert_eq!(store.seal_and_rotate_control_log(&preview)?, binding);
    assert_eq!(std::fs::read(&path)?, original_bytes);
    let stale = command_request(2, 1);
    assert!(
        futures::executor::block_on(store.reserve(
            stale.admission.reservation_key(&stale.envelope.command_id),
            sigil_application::command_fingerprint(&stale)?,
            stale
        ))
        .is_err()
    );
    let mut next = command_request(3, 1);
    next.admission.command_journal = Some(binding.clone());
    assert_eq!(
        futures::executor::block_on(store.reserve(
            next.admission.reservation_key(&next.envelope.command_id),
            sigil_application::command_fingerprint(&next)?,
            next
        ))?,
        RuntimeApplicationReservationAdmission::Reserved
    );
    assert!(matches!(
        futures::executor::block_on(store.reserve(original_key.clone(), fingerprint, original))?,
        RuntimeApplicationReservationAdmission::Existing(_)
    ));
    drop(store);
    let reopened = ManagedApplicationReservationStore::open(writer, "rotate")?;
    assert_eq!(reopened.binding(), binding);
    assert_eq!(
        futures::executor::block_on(reopened.original_command_context(original_key))?
            .expect("retained context")
            .command_journal
            .expect("generation")
            .command_generation,
        0
    );
    assert_eq!(std::fs::read(path)?, original_bytes);
    Ok(())
}

#[test]
fn complete_final_record_missing_separator_is_repaired_without_rewriting_payload()
-> anyhow::Result<()> {
    let fixture = tempfile::tempdir()?;
    let writer = production_writer(fixture.path());
    let request = command_request(1, 1);
    let store = ManagedApplicationReservationStore::open(Arc::clone(&writer), "separator")?;
    futures::executor::block_on(
        store.reserve(
            request
                .admission
                .reservation_key(&request.envelope.command_id),
            sigil_application::command_fingerprint(&request)?,
            request,
        ),
    )?;
    let path = store
        .lease
        .lock()
        .expect("lease")
        .as_ref()
        .expect("journal")
        .path()
        .join("records.jsonl");
    drop(store);
    let complete = std::fs::read(&path)?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)?
        .set_len(complete.len() as u64 - 1)?;
    let reopened = ManagedApplicationReservationStore::open(writer, "separator")?;
    assert_eq!(std::fs::read(path)?, complete);
    let next = command_request(2, 1);
    assert_eq!(
        futures::executor::block_on(reopened.reserve(
            next.admission.reservation_key(&next.envelope.command_id),
            sigil_application::command_fingerprint(&next)?,
            next
        ))?,
        RuntimeApplicationReservationAdmission::Reserved
    );
    Ok(())
}

#[test]
fn replayed_domain_commit_is_a_terminal_admission_before_settlement_index_exists() {
    let key = key();
    let fingerprint = fingerprint();
    let domain = domain_receipt(&key);
    let mut entries = BTreeMap::new();
    apply_journal_operation(
        &mut entries,
        DurableReservationOperation::Reserve {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
        },
    )
    .expect("reserve");
    apply_journal_operation(
        &mut entries,
        DurableReservationOperation::DispatchStarted {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
        },
    )
    .expect("dispatch");
    apply_journal_operation(
        &mut entries,
        DurableReservationOperation::EffectStarted {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
            binding: Box::new(effect_binding(&key)),
        },
    )
    .expect("effect");
    apply_journal_operation(
        &mut entries,
        DurableReservationOperation::DomainCommitted {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
            receipt: Box::new(domain.clone()),
        },
    )
    .expect("domain commit");

    let admission = existing_admission(
        entries.get(&key).expect("record"),
        &key,
        key.command_id.clone(),
        "test".to_owned(),
        fingerprint,
    );
    assert!(matches!(
        admission,
        RuntimeApplicationReservationAdmission::Existing(receipt)
            if matches!(&*receipt, ApplicationCommandReceipt::Settled(receipt) if *receipt == domain)
    ));
}

#[test]
fn pre_effect_replay_requires_repair_instead_of_claiming_in_flight() {
    let key = key();
    let fingerprint = fingerprint();
    let mut entries = BTreeMap::new();
    apply_journal_operation(
        &mut entries,
        DurableReservationOperation::Reserve {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
        },
    )
    .expect("reserve");

    for expected_phase in [
        CommandLifecyclePhase::Reserved,
        CommandLifecyclePhase::DispatchStarted,
    ] {
        if expected_phase == CommandLifecyclePhase::DispatchStarted {
            apply_journal_operation(
                &mut entries,
                DurableReservationOperation::DispatchStarted {
                    key: key.clone(),
                    fingerprint: fingerprint.clone(),
                },
            )
            .expect("dispatch marker");
        }
        let admission = existing_admission(
            entries.get(&key).expect("record"),
            &key,
            key.command_id.clone(),
            "test".to_owned(),
            fingerprint.clone(),
        );
        assert!(matches!(
            admission,
            RuntimeApplicationReservationAdmission::Existing(receipt)
                if matches!(&*receipt, ApplicationCommandReceipt::Uncertain(receipt)
                    if receipt.recovery.phase == expected_phase)
        ));
    }
}

#[test]
fn journal_replay_rejects_non_monotonic_transition_and_unknown_schema() {
    let key = key();
    let mut entries = BTreeMap::new();
    apply_journal_operation(
        &mut entries,
        DurableReservationOperation::Reserve {
            key: key.clone(),
            fingerprint: fingerprint(),
        },
    )
    .expect("reserve");
    let error = apply_journal_operation(
        &mut entries,
        DurableReservationOperation::DomainCommitted {
            key: key.clone(),
            fingerprint: fingerprint(),
            receipt: Box::new(domain_receipt(&key)),
        },
    )
    .expect_err("domain commit before effect");
    assert!(matches!(error, ApplicationError::CorruptProjection(_)));

    let unknown = serde_json::json!({
        "schema_version": APPLICATION_RESERVATION_SCHEMA_VERSION + 1,
        "operation": { "type": "reserve", "value": { "key": {}, "fingerprint": "bad" } }
    });
    let error = decode_entries(serde_json::to_string(&unknown).expect("json").as_bytes())
        .expect_err("unknown schema");
    assert!(matches!(error, ApplicationError::CorruptProjection(_)));
}

#[test]
fn legacy_snapshot_is_not_silently_migrated() {
    let legacy = serde_json::json!({
        "schema_version": 1,
        "entries": []
    });
    let error = decode_entries(
        serde_json::to_string(&legacy)
            .expect("legacy json")
            .as_bytes(),
    )
    .expect_err("legacy format must fail closed");
    assert!(matches!(error, ApplicationError::CorruptProjection(_)));
}

#[test]
fn recovery_preview_counts_complete_prefix_and_bounds_unknown_tail_impact() -> anyhow::Result<()> {
    use sigil_application::SessionScopeId;
    use std::io::Write;
    let fixture = tempfile::tempdir()?;
    let writer = production_writer(fixture.path());
    let store = ManagedApplicationReservationStore::open(Arc::clone(&writer), "impact")?;
    for id in 0..40 {
        let mut request = command_request(id, 1);
        let session = Some(SessionScopeId::new(format!("session-{}", id % 20))?);
        request.admission.scope.session = session.clone();
        request.envelope.expected_frontier.scope.session = session;
        futures::executor::block_on(
            store.reserve(
                request
                    .admission
                    .reservation_key(&request.envelope.command_id),
                sigil_application::command_fingerprint(&request)?,
                request,
            ),
        )?;
    }
    let path = store
        .lease
        .lock()
        .expect("lease")
        .as_ref()
        .expect("journal")
        .path()
        .join("records.jsonl");
    let prefix = std::fs::read(&path)?;
    drop(store);
    let tail = b"{\"operation\":\"unknown";
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)?
        .write_all(tail)?;
    let original = std::fs::read(&path)?;
    let store = ManagedApplicationReservationStore::open(Arc::clone(&writer), "impact")?;
    let preview = store.preview_control_log_recovery()?;
    assert!(preview.old_file_identity.is_some());
    assert_eq!(preview.impact.verified_prefix_bytes, prefix.len() as u64);
    assert_eq!(preview.impact.verified_record_count, 40);
    assert_eq!(
        preview.impact.verified_prefix_digest,
        sigil_kernel::sha256_hex(&prefix)
    );
    assert_eq!(preview.impact.known_command_count, 40);
    assert_eq!(preview.impact.affected_scope_count, 20);
    assert_eq!(preview.impact.affected_scopes.len(), 16);
    assert!(preview.impact.scopes_truncated);
    assert_eq!(preview.impact.known_unresolved_count, 40);
    assert_eq!(preview.impact.unresolved_commands.len(), 32);
    assert!(preview.impact.commands_truncated);
    assert!(preview.impact.tail_command_count_unknown);
    assert_eq!(preview.impact.unparsed_tail_bytes, tail.len() as u64);
    assert!(
        preview
            .impact
            .unresolved_commands
            .iter()
            .all(|command| command.phase == CommandLifecyclePhase::Reserved)
    );
    let mut tampered = preview.clone();
    tampered.impact.known_unresolved_count = 0;
    assert!(store.seal_and_rotate_control_log(&tampered).is_err());
    assert_eq!(store.binding().command_generation, 0);
    // A complete confirmation retains all original bytes and activates only this fixed successor.
    assert_eq!(
        store
            .seal_and_rotate_control_log(&preview)?
            .command_generation,
        1
    );
    assert_eq!(std::fs::read(&path)?, original);
    Ok(())
}
