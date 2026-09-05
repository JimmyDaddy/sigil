use super::*;
use sigil_kernel::managed_storage::StorageAdmissionGrantV1;
use sigil_kernel::resource::{
    AuthorityGeneration, ManagedStorageCapabilityFamilyV1, ManagedStorageSemanticOwnerV1,
    OpaqueSessionId, OpaqueStorageGrantId, ResourceJournalScopeV1, ResourceOwnerScopeV1,
};
use sigil_resource_authority::storage::{
    AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
};

fn hash(seed: u8) -> CanonicalHash {
    CanonicalHash::from_bytes([seed; 32])
}

#[test]
fn r71_sw_physical_frontier_distinguishes_snapshots_from_jsonl() {
    assert_eq!(
        managed_physical_record_count(
            StorageWriterChannelV1::AdapterDurableState,
            br#"{"schema_version":1,"events":[]}"#,
        )
        .expect("complete adapter snapshot"),
        1
    );
    assert!(
        managed_physical_record_count(
            StorageWriterChannelV1::AdapterIdempotencyLedger,
            br#"["not-an-object"]"#,
        )
        .is_err()
    );
    assert_eq!(
        managed_physical_record_count(StorageWriterChannelV1::SessionLog, b"{}\n{}\n")
            .expect("complete JSONL"),
        2
    );
    assert!(managed_physical_record_count(StorageWriterChannelV1::SessionLog, b"{}").is_err());
}

fn session_log_grant() -> StorageAdmissionGrantV1 {
    let cutover_manifest_hash = hash(10);
    let source = sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
        cutover_manifest_hash,
        application_generation: 1,
    };
    StorageAdmissionGrantV1 {
        grant_id: OpaqueStorageGrantId::new("g-writer-slog".to_owned()),
        admission_hash: hash(1),
        semantic_owner: ManagedStorageSemanticOwnerV1::SessionLog,
        purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
        purpose_hash: hash(2),
        source_class: sigil_kernel::resource::StorageAdmissionSourceClassV1::ApplicationCutoverRoot,
        source_binding_hash: sigil_resource_authority::storage::admission_source_binding_hash(
            &source,
        ),
        namespace_hash: super::writer_namespace_hash("session-log"),
        journal_scope: ResourceJournalScopeV1::Application,
        journal_scope_hash: hash(4),
        resource_ref: sigil_kernel::resource::ResourceRefV1 {
            resource_id: sigil_kernel::resource::OpaqueResourceId::new(
                "res-writer-slog".to_owned(),
            ),
            kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
            owner_scope: ResourceOwnerScopeV1::Application,
            journal_scope: ResourceJournalScopeV1::Application,
            generation: 1,
        },
        resource_binding_digest: hash(5),
        physical_binding_hash: hash(6),
        resource_kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
        owner_scope: ResourceOwnerScopeV1::Application,
        capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
        retention_policy: sigil_kernel::resource::ResourceRetentionPolicyV1::SessionPolicy,
        quota_profile: sigil_kernel::resource::ResourceQuotaProfileV1 {
            class: sigil_kernel::resource::ResourceQuotaClassV1::RuntimeState,
            max_bytes: 1024,
            max_entries: 100,
            max_open_holders: 1,
            max_age_ms: None,
            hard_runtime_enforcement_required: true,
            profile_hash: hash(7),
        },
        semantic_schema: sigil_kernel::resource::OpaqueSemanticSchemaId::new(
            "schema-writer-slog".to_owned(),
        ),
        authority_generation: AuthorityGeneration {
            epoch: 1,
            instance_hash: hash(8),
        },
        journal_admission_sequence: 1,
        grant_hash: hash(9),
    }
}

fn adapter(anchor: &Path, table: AuthorityStorageGrantTableV1) -> ManagedStorageWriterAdapterV1 {
    let service: std::sync::Arc<dyn ManagedStorageServiceV1> =
        std::sync::Arc::new(AuthorityManagedStorageServiceV1::new(
            table,
            AuthorityGeneration {
                epoch: 1,
                instance_hash: hash(8),
            },
        ));
    ManagedStorageWriterAdapterV1::new(service, anchor.to_path_buf(), hash(10))
}

/// Existing-only continuation is a production journal protocol, never a no-journal adapter
/// shortcut. All recovery/mutation fixtures below start from this same real authority service.
fn journal_backed_session_log_service(
    anchor: &Path,
) -> std::sync::Arc<AuthorityManagedStorageServiceV1> {
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(session_log_grant()).expect("register");
    std::sync::Arc::new(
        AuthorityManagedStorageServiceV1::new_with_journal(
            table,
            AuthorityGeneration {
                epoch: 1,
                instance_hash: hash(8),
            },
            anchor.join("authority-resources.journal.json"),
            hash(0x91),
            hash(0x92),
        )
        .expect("journal-backed authority service"),
    )
}

fn journal_backed_session_log_writer_with_service(
    anchor: &Path,
    service: std::sync::Arc<AuthorityManagedStorageServiceV1>,
) -> ManagedStorageWriterAdapterV1 {
    use sigil_kernel::capability_issuer::KernelCapabilityBrokerV1;

    ManagedStorageWriterAdapterV1::with_storage_issuer(
        service,
        anchor.to_path_buf(),
        hash(10),
        std::sync::Arc::new(KernelCapabilityBrokerV1::new()),
    )
}

fn journal_backed_session_log_writer(anchor: &Path) -> ManagedStorageWriterAdapterV1 {
    journal_backed_session_log_writer_with_service(
        anchor,
        journal_backed_session_log_service(anchor),
    )
}

#[test]
fn r71_sw_session_log_batch_round_trip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(session_log_grant()).expect("register");
    let writer = adapter(dir.path(), table);
    let lease = writer
        .acquire(StorageWriterChannelV1::SessionLog)
        .expect("acquire");
    assert_eq!(lease.channel(), StorageWriterChannelV1::SessionLog);
    assert!(lease.path().ends_with("managed/session-log"));
    writer
        .write_record(&lease, b"{\"seq\":1}")
        .expect("write 1");
    writer
        .write_record(&lease, b"{\"seq\":2}")
        .expect("write 2");
    let content = std::fs::read_to_string(lease.path().join("records.jsonl")).expect("read");
    assert_eq!(content, "{\"seq\":1}\n{\"seq\":2}\n");
    let receipt = writer.finalize(lease).expect("finalize");
    assert_eq!(
        receipt.capability_family,
        ManagedStorageCapabilityFamilyV1::AppendLog
    );
}

#[test]
fn r71_sw_unregistered_family_fails_admission() {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer = adapter(dir.path(), AuthorityStorageGrantTableV1::new());
    let error = writer
        .acquire(StorageWriterChannelV1::SessionLog)
        .expect_err("no grant");
    assert!(matches!(
        error,
        ManagedStorageWriterErrorV1::AdmissionFailed(_)
    ));
}

#[test]
fn r71_sw_leaf_permissions_owner_only() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(session_log_grant()).expect("register");
        let writer = adapter(dir.path(), table);
        let lease = writer
            .acquire(StorageWriterChannelV1::SessionLog)
            .expect("acquire");
        writer.write_record(&lease, b"{\"seq\":1}").expect("write");
        let dir_meta = std::fs::symlink_metadata(lease.path()).expect("dir meta");
        assert_eq!(dir_meta.permissions().mode() & 0o077, 0);
        let file_meta =
            std::fs::symlink_metadata(lease.path().join("records.jsonl")).expect("file meta");
        assert_eq!(file_meta.permissions().mode() & 0o077, 0);
    }
}

#[test]
fn r71_sw_finalize_twice_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(session_log_grant()).expect("register");
    let writer = adapter(dir.path(), table);
    let lease = writer
        .acquire(StorageWriterChannelV1::SessionLog)
        .expect("acquire");
    let path = lease.path().to_path_buf();
    let channel = lease.channel();
    let namespace_digest = lease.namespace_digest();
    writer.finalize(lease).expect("first finalize");
    // A second finalize of the same namespace is refused by the authority.
    let error = writer
        .finalize(ManagedStorageWriterLeaseV1 {
            handle: ManagedStorageNamespaceHandleV1::new(
                sigil_kernel::resource::OpaqueKernelCapabilityHandleId::new(
                    "handle-storage-1".to_owned(),
                ),
                namespace_digest,
                ManagedStorageCapabilityFamilyV1::AppendLog,
                sigil_kernel::resource::OpaqueKernelCapabilityAuthenticatorV1::new(
                    "auth-storage-1".to_owned(),
                ),
            ),
            path,
            channel,
        })
        .expect_err("second finalize");
    assert!(matches!(
        error,
        ManagedStorageWriterErrorV1::FinalizeFailed(_)
    ));
}
#[test]
fn r71_sw_broker_backed_writer_uses_production_namespace() {
    use sigil_kernel::capability_issuer::KernelCapabilityBrokerV1;
    let dir = tempfile::tempdir().expect("tempdir");
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(session_log_grant()).expect("register");
    let service: std::sync::Arc<dyn ManagedStorageServiceV1> =
        std::sync::Arc::new(AuthorityManagedStorageServiceV1::new(
            table,
            AuthorityGeneration {
                epoch: 1,
                instance_hash: hash(8),
            },
        ));
    let broker = std::sync::Arc::new(KernelCapabilityBrokerV1::new());
    let writer = ManagedStorageWriterAdapterV1::with_storage_issuer(
        service.clone(),
        dir.path().to_path_buf(),
        hash(10),
        broker.clone(),
    );
    // A startup probe runs first: its namespace is dedicated and never the production one.
    let capability =
        sigil_kernel::managed_storage::ValidatedStorageAdmissionCapabilityV1::startup_probe();
    let request = sigil_kernel::managed_storage::ManagedStorageAdmissionRequestV1 {
        semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLog,
        capability_family: sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog,
        purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
        source: sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
            cutover_manifest_hash: hash(10),
            application_generation: 1,
        },
        owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Session(OpaqueSessionId::new(
            "s-1".to_owned(),
        )),
        journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
    };
    let probe_handle = service
        .admit_namespace(request, capability)
        .expect("probe admit");
    assert_ne!(probe_handle.namespace_hash, hash(3));
    let probe_ns = probe_handle.namespace_hash;
    service
        .finalize_namespace(probe_handle, "probe".to_owned())
        .expect("probe finalize");
    // The broker-backed writer batch binds a claim-scoped namespace (distinct from the
    // probe claim) and works after the probe finalized its own namespace.
    let lease = writer
        .acquire(StorageWriterChannelV1::SessionLog)
        .expect("acquire");
    assert_ne!(lease.namespace_digest(), probe_ns);
    assert_ne!(
        lease.namespace_digest(),
        CanonicalHash::from_bytes([0u8; 32])
    );
    writer.write_record(&lease, b"seq=1").expect("write");
    writer.finalize(lease).expect("finalize");
}

#[test]
fn r71_sw_finalize_commits_physical_frontier_before_settlement() {
    use sigil_kernel::capability_issuer::KernelCapabilityBrokerV1;
    let dir = tempfile::tempdir().expect("tempdir");
    let journal_path = dir.path().join("authority-resources.journal.json");
    let bootstrap_manifest_hash = hash(0x91);
    let journal_instance_hash = hash(0x92);
    let header = sigil_resource_authority::journal::ResourceJournalHeaderV1 {
        schema_version: 1,
        shard_name: "application-resources".to_owned(),
        bootstrap_manifest_hash,
        journal_instance_hash,
        header_hash: crate::r71_shadow_planner::canonical_digest(
            format!(
                "{:?}",
                (
                    "application-resources",
                    bootstrap_manifest_hash,
                    journal_instance_hash
                )
            )
            .as_bytes(),
        ),
    };
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(session_log_grant()).expect("register");
    let service = std::sync::Arc::new(
        AuthorityManagedStorageServiceV1::new_with_journal(
            table,
            AuthorityGeneration {
                epoch: 1,
                instance_hash: hash(8),
            },
            &journal_path,
            header.bootstrap_manifest_hash,
            header.journal_instance_hash,
        )
        .expect("authority service"),
    );
    let broker = std::sync::Arc::new(KernelCapabilityBrokerV1::new());
    let writer = ManagedStorageWriterAdapterV1::with_storage_issuer(
        service.clone(),
        dir.path().to_path_buf(),
        hash(10),
        broker,
    );
    let lease = writer
        .acquire(StorageWriterChannelV1::SessionLog)
        .expect("acquire");
    let durable_binding = lease
        .handle
        .durable_admission()
        .expect("journal-backed handle binding");
    let marker: serde_json::Value = serde_json::from_slice(
        &std::fs::read(lease.path().join("authority-admission.json")).expect("admission marker"),
    )
    .expect("marker json");
    assert_eq!(marker["schema_version"], 2);
    assert_eq!(
        marker["grant_hash"],
        serde_json::to_value(durable_binding.grant_hash).expect("grant hash json")
    );
    assert_eq!(
        marker["admission_sequence"],
        durable_binding.admission_sequence
    );
    assert_eq!(
        marker["admission_record_hash"],
        serde_json::to_value(durable_binding.admission_record_hash).expect("record hash json")
    );
    let namespace_hash = lease.namespace_digest();
    let grant_hash = session_log_grant().grant_hash;
    writer.write_record(&lease, b"seq=1").expect("write");
    let stale_frontier = writer.physical_frontier(&lease).expect("physical frontier");
    assert_eq!(
        stale_frontier,
        writer.physical_frontier(&lease).expect("stable frontier")
    );
    writer.write_record(&lease, b"seq=2").expect("second write");
    let current_frontier = writer.physical_frontier(&lease).expect("current frontier");
    let shadow_handle = sigil_kernel::managed_storage::ManagedStorageNamespaceHandleV1::new_durable(
        lease.handle.handle_id.clone(),
        lease.handle.namespace_hash,
        lease.handle.capability_family,
        durable_binding,
        sigil_kernel::resource::OpaqueKernelCapabilityAuthenticatorV1::new(
            "test-shadow".to_owned(),
        ),
    );
    let receipt = service
        .finalize_namespace_with_physical_frontier(
            shadow_handle,
            current_frontier.0,
            current_frontier.1,
            current_frontier.2,
            "writer-batch-complete".to_owned(),
        )
        .expect("finalize");
    assert!(matches!(
        writer.write_record(&lease, b"post-settlement"),
        Err(ManagedStorageWriterErrorV1::LeaseRejected(_))
    ));
    assert_eq!(receipt.committed_sequence_or_version, Some(4));
    let journal =
        sigil_resource_authority::journal::ResourceJournalFileV1::open(journal_path, header)
            .expect("reopen journal");
    assert_eq!(journal.tail().expect("tail").sequence, 4);
    let observations = journal.storage_physical_frontier_records(grant_hash, namespace_hash);
    assert_eq!(observations.len(), 1);
    let observation = &observations[0];
    assert_eq!(receipt.physical_frontier_hash, Some(observation.4));
    assert_eq!(
        receipt.physical_observation_record_hash,
        Some(observation.0.record_hash)
    );
    assert_eq!(
        journal.storage_settlement_binding(grant_hash),
        Some((Some(observation.4), Some(observation.0.record_hash)))
    );
}
#[test]
fn r71_sw_named_acquire_per_session_and_unsafe_key_rejected() {
    use sigil_kernel::capability_issuer::KernelCapabilityBrokerV1;
    let dir = tempfile::tempdir().expect("tempdir");
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(session_log_grant()).expect("register");
    let service: std::sync::Arc<dyn ManagedStorageServiceV1> =
        std::sync::Arc::new(AuthorityManagedStorageServiceV1::new(
            table,
            AuthorityGeneration {
                epoch: 1,
                instance_hash: hash(8),
            },
        ));
    let broker = std::sync::Arc::new(KernelCapabilityBrokerV1::new());
    let writer = ManagedStorageWriterAdapterV1::with_storage_issuer(
        service.clone(),
        dir.path().to_path_buf(),
        hash(10),
        broker,
    );
    let lease_a = writer
        .acquire_named(StorageWriterChannelV1::SessionLog, "session-abc")
        .expect("named a");
    assert!(lease_a.path().ends_with("session-log/session-abc"));
    writer.write_record(&lease_a, b"seq=1").expect("write");
    writer.finalize(lease_a).expect("finalize a");
    let lease_b = writer
        .acquire_named(StorageWriterChannelV1::SessionLog, "session-def")
        .expect("named b");
    writer.finalize(lease_b).expect("finalize b");
    // Unsafe sub-key rejected before any filesystem access.
    let error = writer
        .acquire_named(StorageWriterChannelV1::SessionLog, "../escape")
        .expect_err("unsafe");
    assert!(matches!(
        error,
        ManagedStorageWriterErrorV1::LeafEscapesAnchor
    ));
}

fn finalized_kernel_session_log_namespace(
    writer: &ManagedStorageWriterAdapterV1,
    key: &str,
) -> std::path::PathBuf {
    let lease = writer
        .acquire_named(StorageWriterChannelV1::SessionLog, key)
        .expect("initial named admission");
    let path = lease.path().to_path_buf();
    {
        let store = sigil_kernel::JsonlSessionStore::new(path.join("records.jsonl"))
            .expect("initial kernel session store");
        store
            .append_event(
                sigil_kernel::DurableEventType::RunStatusChanged,
                sigil_kernel::EventClass::Critical,
                serde_json::json!({
                    "run_status": "running",
                    "terminal_reason": "in_progress",
                    "tool_calls": 0
                }),
            )
            .expect("initial durable event");
    }
    writer.finalize(lease).expect("initial finalize");
    path
}

#[test]
fn r71_sw_existing_session_log_recovery_reopens_bytes_without_initializing_artifacts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer = journal_backed_session_log_writer(dir.path());
    let key = "plan-review-research";
    let path = finalized_kernel_session_log_namespace(&writer, key);
    let record_path = path.join("records.jsonl");
    let marker_path = path.join("authority-admission.json");
    let original_record = std::fs::read(&record_path).expect("original record");
    let original_marker = std::fs::read(&marker_path).expect("original marker");

    let recovery = writer
        .acquire_existing_session_log_for_recovery(key)
        .expect("existing-only admission");
    assert_eq!(
        writer
            .read_existing_session_log_bytes(&recovery, 1024)
            .expect("recovery bytes"),
        original_record
    );
    writer
        .finalize_existing_session_log_recovery(recovery)
        .expect("recovery finalize");

    assert_eq!(
        std::fs::read(&record_path).expect("record remains"),
        original_record
    );
    assert_eq!(
        std::fs::read(&marker_path).expect("marker remains"),
        original_marker
    );
    assert!(
        !dir.path()
            .join("managed/artifact-staging")
            .join(key)
            .exists()
    );
    assert!(!dir.path().join("managed/artifact-store").join(key).exists());
}

#[test]
fn r71_sw_existing_session_log_mutation_reopens_only_a_prior_kernel_stream() {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer = journal_backed_session_log_writer(dir.path());
    let key = "plan-review-research-mutation";
    let path = finalized_kernel_session_log_namespace(&writer, key);
    let record_path = path.join("records.jsonl");
    let marker_path = path.join("authority-admission.json");
    let original = std::fs::read(&record_path).expect("original record");
    let marker = std::fs::read(&marker_path).expect("original marker");

    let lease = writer
        .acquire_existing_session_log_for_mutation(key)
        .expect("existing mutation admission");
    writer
        .with_existing_session_log_mutation(&lease, |store| {
            assert_eq!(
                store
                    .read_event_records_writer()
                    .expect("original records")
                    .len(),
                1
            );
            store
                .append_event(
                    sigil_kernel::DurableEventType::RunFinalized,
                    sigil_kernel::EventClass::Critical,
                    serde_json::json!({
                        "run_status": "completed",
                        "terminal_reason": "completed",
                        "tool_calls": 0
                    }),
                )
                .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
            Ok::<_, ManagedStorageWriterErrorV1>(())
        })
        .expect("existing kernel session store");
    writer
        .finalize_existing_session_log_mutation(lease)
        .expect("existing mutation finalize");

    let after = std::fs::read(&record_path).expect("updated record");
    assert!(after.starts_with(&original));
    assert_eq!(
        sigil_kernel::JsonlSessionStore::read_event_records(&record_path)
            .expect("updated records")
            .len(),
        2
    );
    assert_eq!(std::fs::read(&marker_path).expect("marker remains"), marker);
    assert!(
        !dir.path()
            .join("managed/artifact-staging")
            .join(key)
            .exists()
    );
    assert!(!dir.path().join("managed/artifact-store").join(key).exists());
}

#[test]
fn r71_sw_existing_session_log_mutation_rejects_a_stale_continuation_after_foreign_settlement() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first_service = journal_backed_session_log_service(dir.path());
    let writer = journal_backed_session_log_writer_with_service(
        dir.path(),
        std::sync::Arc::clone(&first_service),
    );
    let key = "plan-review-research-stale-continuation";
    let path = finalized_kernel_session_log_namespace(&writer, key);
    let record_path = path.join("records.jsonl");
    let original = std::fs::read(&record_path).expect("original child stream");

    let lease = writer
        .acquire_existing_session_log_for_mutation(key)
        .expect("continuation admission");

    // A second real authority is opened after the continuation admission. Its bridge settlement
    // advances durable state while the first service retains a stale in-memory journal snapshot.
    let foreign = journal_backed_session_log_service(dir.path());
    foreign
        .reconcile_unsettled_storage_grants_with_physical_bridge()
        .expect("foreign continuation settlement");

    let error = writer
        .with_existing_session_log_mutation(&lease, |store| {
            store
                .append_event(
                    sigil_kernel::DurableEventType::RunFinalized,
                    sigil_kernel::EventClass::Critical,
                    serde_json::json!({"status": "must-not-append"}),
                )
                .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
            Ok::<_, ManagedStorageWriterErrorV1>(())
        })
        .expect_err("stale continuation must fail before the child write callback");
    assert!(matches!(
        error,
        ManagedStorageWriterErrorV1::LeaseRejected(_)
    ));
    assert_eq!(
        std::fs::read(&record_path).expect("child stream remains unchanged"),
        original
    );
}

#[test]
fn r71_sw_existing_session_log_mutation_refuses_missing_kernel_writer_lock_before_admission() {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer = journal_backed_session_log_writer(dir.path());
    let key = "plan-review-research-mutation-missing-writer-lock";
    let path = finalized_kernel_session_log_namespace(&writer, key);
    let writer_lock = path.join("records.jsonl.writer-lock");
    std::fs::remove_file(&writer_lock).expect("remove kernel writer lock");

    let error = writer
        .acquire_existing_session_log_for_mutation(key)
        .expect_err("missing existing kernel writer lock must fail before admission");
    assert!(error.to_string().contains("session writer lock is missing"));
    assert!(!writer_lock.exists());
}

#[test]
fn r71_sw_existing_session_log_recovery_refuses_missing_namespace_before_admission() {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer = journal_backed_session_log_writer(dir.path());
    let key = "missing-plan-review-research";
    let path = writer
        .managed_named_leaf_path(StorageWriterChannelV1::SessionLog, key)
        .expect("managed path");

    let error = writer
        .acquire_existing_session_log_for_recovery(key)
        .expect_err("missing namespace must not be initialized");
    assert!(error.to_string().contains("namespace is missing"));
    assert!(!path.exists());

    // A normal subsequent admission proves the rejected recovery never consumed a holder.
    let lease = writer
        .acquire_named(StorageWriterChannelV1::SessionLog, key)
        .expect("normal admission remains available");
    writer.finalize(lease).expect("normal finalize");
}

#[test]
fn r71_sw_existing_session_log_recovery_refuses_missing_record_without_reseeding() {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer = journal_backed_session_log_writer(dir.path());
    let key = "missing-plan-review-record";
    let path = finalized_kernel_session_log_namespace(&writer, key);
    let record_path = path.join("records.jsonl");
    std::fs::remove_file(&record_path).expect("remove fixture record");

    let error = writer
        .acquire_existing_session_log_for_recovery(key)
        .expect_err("missing record must not be initialized");
    assert!(error.to_string().contains("record is missing"));
    assert!(!record_path.exists());

    // As above, no recovery admission is left live when validation fails before admission.
    let lease = writer
        .acquire_named(StorageWriterChannelV1::SessionLog, key)
        .expect("normal admission remains available");
    writer.finalize(lease).expect("normal finalize");
}

#[test]
fn r71_sw_existing_session_log_recovery_refuses_missing_lock_without_creating_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer = journal_backed_session_log_writer(dir.path());
    let key = "missing-plan-review-lock";
    let path = finalized_kernel_session_log_namespace(&writer, key);
    let lock_path = path.join(".authority-storage.lock");
    std::fs::remove_file(&lock_path).expect("remove fixture lock");

    let error = writer
        .acquire_existing_session_log_for_recovery(key)
        .expect_err("missing recovery lock must not be created");
    assert!(error.to_string().contains("lock is missing"));
    assert!(!lock_path.exists());

    let lease = writer
        .acquire_named(StorageWriterChannelV1::SessionLog, key)
        .expect("normal admission remains available");
    writer.finalize(lease).expect("normal finalize");
}

#[cfg(unix)]
#[test]
fn r71_sw_existing_session_log_recovery_refuses_symlinked_record_without_following_it() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("tempdir");
    let writer = journal_backed_session_log_writer(dir.path());
    let key = "symlinked-plan-review-record";
    let path = finalized_kernel_session_log_namespace(&writer, key);
    let record_path = path.join("records.jsonl");
    let external = dir.path().join("external-record.jsonl");
    std::fs::write(&external, b"outside").expect("external fixture");
    std::fs::remove_file(&record_path).expect("remove fixture record");
    symlink(&external, &record_path).expect("record symlink");

    assert!(
        writer
            .acquire_existing_session_log_for_recovery(key)
            .is_err()
    );
    assert!(
        std::fs::symlink_metadata(&record_path)
            .expect("symlink remains")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read(&external).expect("external remains"),
        b"outside"
    );
}

#[test]
fn r71_sw_artifact_staging_and_store_are_separate_authority_roots() {
    use sigil_kernel::session::ToolArtifactSensitivity;
    let dir = tempfile::tempdir().expect("tempdir");
    let mut table = AuthorityStorageGrantTableV1::new();
    table
        .register(grant_for_channel_with_context(
            StorageWriterChannelV1::ArtifactStaging,
            0x81,
            AuthorityGeneration {
                epoch: 1,
                instance_hash: hash(0x83),
            },
            hash(0x84),
        ))
        .expect("staging grant");
    table
        .register(grant_for_channel_with_context(
            StorageWriterChannelV1::ArtifactStore,
            0x82,
            AuthorityGeneration {
                epoch: 1,
                instance_hash: hash(0x83),
            },
            hash(0x84),
        ))
        .expect("store grant");
    let service: std::sync::Arc<dyn ManagedStorageServiceV1> =
        std::sync::Arc::new(AuthorityManagedStorageServiceV1::new(
            table,
            AuthorityGeneration {
                epoch: 1,
                instance_hash: hash(0x83),
            },
        ));
    let writer = ManagedStorageWriterAdapterV1::new(service, dir.path().to_path_buf(), hash(0x84));
    let staging = writer
        .acquire_named(StorageWriterChannelV1::ArtifactStaging, "session-artifact")
        .expect("staging lease");
    let store = writer
        .acquire_named(StorageWriterChannelV1::ArtifactStore, "session-artifact")
        .expect("store lease");
    let session_path = dir.path().join("session.jsonl");
    let artifact_store = sigil_kernel::ToolArtifactStore::for_session_path_with_roots(
        &session_path,
        store.path().to_path_buf(),
        staging.path().to_path_buf(),
    );
    let descriptor = artifact_store
        .capture_text(
            "call-1",
            "shell",
            "managed artifact",
            ToolArtifactSensitivity::Ordinary,
        )
        .expect("artifact should publish");
    assert!(descriptor.retrieval_available());
    assert!(artifact_store.root().join("refs").exists());
    assert!(artifact_store.staging_root().join("staging").exists());
    assert!(!dir.path().join("session").join("artifacts").exists());
    writer.finalize(store).expect("store finalize");
    writer.finalize(staging).expect("staging finalize");
}
