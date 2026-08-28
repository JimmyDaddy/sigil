use super::*;

fn grant() -> StorageAdmissionGrantV1 {
    StorageAdmissionGrantV1 {
        grant_id: OpaqueStorageGrantId::new("grant-1".to_owned()),
        admission_hash: CanonicalHash::from_bytes([1u8; 32]),
        semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLog,
        purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
        purpose_hash: CanonicalHash::from_bytes([2u8; 32]),
        source_class: sigil_kernel::resource::StorageAdmissionSourceClassV1::ApplicationCutoverRoot,
        source_binding_hash: CanonicalHash::from_bytes([9u8; 32]),
        namespace_hash: CanonicalHash::from_bytes([3u8; 32]),
        journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
        journal_scope_hash: CanonicalHash::from_bytes([4u8; 32]),
        resource_ref: sigil_kernel::resource::ResourceRefV1 {
            resource_id: sigil_kernel::resource::OpaqueResourceId::new("resource-1".to_owned()),
            kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
            owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
            journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
            generation: 1,
        },
        resource_binding_digest: CanonicalHash::from_bytes([5u8; 32]),
        physical_binding_hash: CanonicalHash::from_bytes([6u8; 32]),
        resource_kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
        owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
        capability_family: sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog,
        retention_policy: sigil_kernel::resource::ResourceRetentionPolicyV1::SessionPolicy,
        quota_profile: sigil_kernel::resource::ResourceQuotaProfileV1 {
            class: sigil_kernel::resource::ResourceQuotaClassV1::RuntimeState,
            max_bytes: 1024,
            max_entries: 100,
            max_open_holders: 1,
            max_age_ms: None,
            hard_runtime_enforcement_required: true,
            profile_hash: CanonicalHash::from_bytes([7u8; 32]),
        },
        semantic_schema: sigil_kernel::resource::OpaqueSemanticSchemaId::new("schema-1".to_owned()),
        authority_generation: AuthorityGeneration {
            epoch: 1,
            instance_hash: CanonicalHash::from_bytes([8u8; 32]),
        },
        journal_admission_sequence: 1,
        grant_hash: CanonicalHash::from_bytes([9u8; 32]),
    }
}

#[test]
fn r71_storage_grant_table_rejects_duplicate_grant() {
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant()).expect("first");
    let error = table.register(grant()).expect_err("duplicate must fail");
    assert!(matches!(error, ManagedStorageErrorV1::CapabilityMismatch));
}

#[test]
fn r71_storage_key_registry_rejects_duplicate_key_id() {
    let mut registry = AuthorityLogicalKeyRegistryV1::default();
    let key = OpaqueStorageKeyIdV1::new("key-1".to_owned());
    registry
        .reserve(
            key.clone(),
            sigil_kernel::resource::StorageLogicalKeyKindV1::Object,
        )
        .expect("first");
    let error = registry
        .reserve(key, sigil_kernel::resource::StorageLogicalKeyKindV1::Stream)
        .expect_err("duplicate");
    assert!(matches!(error, ManagedStorageErrorV1::DuplicateClaim));
}

#[test]
fn r71_storage_receipt_shape_is_closed() {
    let receipt = sample_storage_receipt();
    assert_eq!(
        receipt.semantic_owner,
        sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLifecycleLog
    );
    assert_eq!(receipt.committed_sequence_or_version, Some(7));
}

fn storage_test_request() -> ManagedStorageAdmissionRequestV1 {
    ManagedStorageAdmissionRequestV1 {
        semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLog,
        capability_family: sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog,
        purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
        source: sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
            cutover_manifest_hash: CanonicalHash::from_bytes([9u8; 32]),
            application_generation: 1,
        },
        owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
        journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
    }
}

fn storage_test_header() -> crate::journal::ResourceJournalHeaderV1 {
    crate::journal::ResourceJournalHeaderV1 {
        schema_version: 1,
        shard_name: "application-resources".to_owned(),
        bootstrap_manifest_hash: CanonicalHash::from_bytes([1u8; 32]),
        journal_instance_hash: CanonicalHash::from_bytes([2u8; 32]),
        header_hash: super::hash_debug(&(
            "application-resources",
            CanonicalHash::from_bytes([1u8; 32]),
            CanonicalHash::from_bytes([2u8; 32]),
        )),
    }
}

#[test]
fn r71_storage_rehydrates_pending_admission_requires_physical_bridge() {
    let directory = tempfile::tempdir().expect("journal directory");
    let path = directory.path().join("authority-resources.journal.json");
    let authority_generation = grant().authority_generation;
    let journal_header = storage_test_header();
    {
        let mut journal =
            crate::journal::ResourceJournalFileV1::open(&path, journal_header.clone())
                .expect("journal");
        journal
            .append_event(
                crate::journal::ResourceJournalEventV1::StorageNamespaceAdmitted {
                    grant_hash: grant().grant_hash,
                    handle_id: "rehydrated-handle".to_owned(),
                    namespace_hash: CanonicalHash::from_bytes([3u8; 32]),
                    grant: Box::new(grant()),
                    request: Box::new(storage_test_request()),
                },
            )
            .expect("admission");
    }
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant()).expect("grant");
    let service = AuthorityManagedStorageServiceV1::new_with_journal(
        table,
        authority_generation,
        &path,
        journal_header.bootstrap_manifest_hash,
        journal_header.journal_instance_hash,
    )
    .expect("rehydrate");
    assert!(matches!(
        service.require_startup_reconciliation(),
        Err(ManagedStorageErrorV1::JournalUnavailable)
    ));
    assert!(matches!(
        service.reconcile_unsettled_storage_grants("recovered-cleanup"),
        Err(ManagedStorageErrorV1::JournalUnavailable)
    ));

    let mut reopened_table = AuthorityStorageGrantTableV1::new();
    reopened_table.register(grant()).expect("grant");
    let reopened = AuthorityManagedStorageServiceV1::new_with_journal(
        reopened_table,
        authority_generation,
        &path,
        journal_header.bootstrap_manifest_hash,
        journal_header.journal_instance_hash,
    )
    .expect("reopen");
    assert!(matches!(
        reopened.reconcile_unsettled_storage_grants("already-settled"),
        Err(ManagedStorageErrorV1::JournalUnavailable)
    ));
}

#[test]
fn r71_storage_rehydrates_source_bound_pending_grant_for_recovery_only() {
    let directory = tempfile::tempdir().expect("journal directory");
    let path = directory.path().join("authority-resources.journal.json");
    let header = storage_test_header();
    let historical = grant();
    {
        let mut journal =
            crate::journal::ResourceJournalFileV1::open(&path, header.clone()).expect("journal");
        journal
            .append_event(
                crate::journal::ResourceJournalEventV1::StorageNamespaceAdmitted {
                    grant_hash: historical.grant_hash,
                    handle_id: "pending-binding-mismatch".to_owned(),
                    namespace_hash: CanonicalHash::from_bytes([3u8; 32]),
                    grant: Box::new(historical.clone()),
                    request: Box::new(storage_test_request()),
                },
            )
            .expect("admission");
    }

    let mut current = historical.clone();
    current.source_binding_hash = CanonicalHash::from_bytes([0xabu8; 32]);
    current.grant_hash = CanonicalHash::from_bytes([0xacu8; 32]);
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(current.clone()).expect("current grant");
    let service = AuthorityManagedStorageServiceV1::new_with_journal(
        table,
        historical.authority_generation,
        &path,
        header.bootstrap_manifest_hash,
        header.journal_instance_hash,
    )
    .expect("source-bound rollover is recoverable historical state");
    assert!(matches!(
        service.require_startup_reconciliation(),
        Err(ManagedStorageErrorV1::JournalUnavailable)
    ));
    assert!(matches!(
        service.reconcile_unsettled_storage_grants_with_physical_bridge(),
        Err(ManagedStorageErrorV1::JournalUnavailable)
    ));

    let broker = sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new();
    let current_capability = broker
        .issue_storage_namespace_capability(
            broker.seal_storage_namespace_proof(current.capability_family, current.namespace_hash),
        )
        .expect("current capability");
    assert!(matches!(
        service.admit_namespace(storage_test_request(), current_capability),
        Err(ManagedStorageErrorV1::JournalUnavailable)
    ));
}

#[test]
fn r71_storage_quarantines_ambiguous_legacy_alias_without_selecting_or_deleting_data() {
    let directory = tempfile::tempdir().expect("journal directory");
    let path = directory.path().join("authority-resources.journal.json");
    let header = storage_test_header();
    let historical = grant();
    let namespace_hash = CanonicalHash::from_bytes([3u8; 32]);
    {
        let mut journal =
            crate::journal::ResourceJournalFileV1::open(&path, header.clone()).expect("journal");
        journal
            .append_event(
                crate::journal::ResourceJournalEventV1::StorageNamespaceAdmitted {
                    grant_hash: historical.grant_hash,
                    handle_id: "proof-4".to_owned(),
                    namespace_hash,
                    grant: Box::new(historical.clone()),
                    request: Box::new(storage_test_request()),
                },
            )
            .expect("admission");
    }

    let first = directory.path().join("managed/session-log/session-a");
    let second = directory.path().join("managed/session-log/session-b");
    for (candidate, records) in [
        (&first, b"{\"seq\":1}\n".as_slice()),
        (&second, b"{\"seq\":2}\n".as_slice()),
    ] {
        std::fs::create_dir_all(candidate).expect("legacy candidate");
        std::fs::write(
            candidate.join("authority-admission.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "handle_id": "proof-4",
                "namespace_hash": namespace_hash,
            }))
            .expect("marker json"),
        )
        .expect("marker");
        std::fs::write(candidate.join("records.jsonl"), records).expect("records");
    }
    let first_before = std::fs::read(first.join("records.jsonl")).expect("first before");
    let second_before = std::fs::read(second.join("records.jsonl")).expect("second before");

    let mut current = historical.clone();
    current.source_binding_hash = CanonicalHash::from_bytes([0xabu8; 32]);
    current.grant_hash = CanonicalHash::from_bytes([0xacu8; 32]);
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(current.clone()).expect("current grant");
    let service = AuthorityManagedStorageServiceV1::new_with_journal(
        table,
        historical.authority_generation,
        &path,
        header.bootstrap_manifest_hash,
        header.journal_instance_hash,
    )
    .expect("recoverable historical admission");
    let receipts = service
        .reconcile_unsettled_storage_grants_with_physical_bridge()
        .expect("quarantine legacy alias");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].committed_sequence_or_version, Some(3));
    assert_eq!(receipts[0].physical_frontier_hash, None);
    service
        .require_startup_reconciliation()
        .expect("exact admission blocker cleared");
    assert_eq!(
        std::fs::read(first.join("records.jsonl")).expect("first retained"),
        first_before
    );
    assert_eq!(
        std::fs::read(second.join("records.jsonl")).expect("second retained"),
        second_before
    );
    drop(service);

    let mut reopened_table = AuthorityStorageGrantTableV1::new();
    reopened_table.register(current).expect("current grant");
    let reopened = AuthorityManagedStorageServiceV1::new_with_journal(
        reopened_table,
        historical.authority_generation,
        &path,
        header.bootstrap_manifest_hash,
        header.journal_instance_hash,
    )
    .expect("terminal quarantine replays");
    reopened
        .require_startup_reconciliation()
        .expect("quarantine remains terminal");
    let journal =
        crate::journal::ResourceJournalFileV1::open(&path, header).expect("reopen durable journal");
    assert!(journal.unsettled_storage_admissions().is_empty());
    let journal_text = std::fs::read_to_string(path).expect("journal text");
    assert!(journal_text.contains("StorageAdmissionAliasQuarantined"));
}

#[test]
fn r71_storage_physical_bridge_replays_complete_seven_record_chain() {
    let directory = tempfile::tempdir().expect("journal directory");
    let path = directory.path().join("authority-resources.journal.json");
    let header = storage_test_header();
    let grant = grant();
    let namespace_hash = CanonicalHash::from_bytes([3u8; 32]);
    {
        let mut journal =
            crate::journal::ResourceJournalFileV1::open(&path, header.clone()).expect("journal");
        journal
            .append_event(
                crate::journal::ResourceJournalEventV1::StorageNamespaceAdmitted {
                    grant_hash: grant.grant_hash,
                    handle_id: "rehydrated-handle".to_owned(),
                    namespace_hash,
                    grant: Box::new(grant.clone()),
                    request: Box::new(storage_test_request()),
                },
            )
            .expect("admission");
    }
    let namespace = directory.path().join("managed/session-log");
    std::fs::create_dir_all(&namespace).expect("managed root");
    std::fs::write(
        namespace.join("authority-admission.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "handle_id": "rehydrated-handle",
            "namespace_hash": namespace_hash,
        }))
        .expect("marker json"),
    )
    .expect("marker");
    std::fs::write(namespace.join("records.jsonl"), b"{\"seq\":1}\n").expect("records");

    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant.clone()).expect("grant");
    let service = AuthorityManagedStorageServiceV1::new_with_journal(
        table,
        grant.authority_generation,
        &path,
        header.bootstrap_manifest_hash,
        header.journal_instance_hash,
    )
    .expect("rehydrate");
    let receipts = service
        .reconcile_unsettled_storage_grants_with_physical_bridge()
        .expect("physical bridge");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].committed_sequence_or_version, Some(8));
    service
        .require_startup_reconciliation()
        .expect("settlement clears startup blocker");

    let journal =
        crate::journal::ResourceJournalFileV1::open(&path, header).expect("reopen journal");
    assert_eq!(journal.storage_recovery_records(grant.grant_hash).len(), 7);
    assert_eq!(
        std::fs::read(namespace.join("records.jsonl")).expect("retained records"),
        b"{\"seq\":1}\n"
    );
}

#[test]
fn r71_storage_restart_recovers_new_admission_after_same_grant_settled_once() {
    use sigil_kernel::capability_issuer::KernelCapabilityBrokerV1;

    let directory = tempfile::tempdir().expect("journal directory");
    let path = directory.path().join("authority-resources.journal.json");
    let header = storage_test_header();
    let grant = grant();
    let broker = KernelCapabilityBrokerV1::new();
    let issue_capability = || {
        broker
            .issue_storage_namespace_capability(
                broker.seal_storage_namespace_proof(grant.capability_family, grant.namespace_hash),
            )
            .expect("capability")
    };
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant.clone()).expect("grant");
    let service = AuthorityManagedStorageServiceV1::new_with_journal(
        table,
        grant.authority_generation,
        &path,
        header.bootstrap_manifest_hash,
        header.journal_instance_hash,
    )
    .expect("service");

    let first = service
        .admit_namespace(storage_test_request(), issue_capability())
        .expect("first admission");
    let first_directory = directory.path().join("managed/session-log/first");
    write_physical_test_namespace(&first_directory, &first, b"{\"seq\":1}\n");
    service
        .finalize_namespace_with_physical_frontier(
            first,
            10,
            1,
            hash_bytes(b"{\"seq\":1}\n"),
            "first-settled".to_owned(),
        )
        .expect("first settlement");

    let second = service
        .admit_namespace(storage_test_request(), issue_capability())
        .expect("second admission");
    let second_directory = directory.path().join("managed/session-log/second");
    write_physical_test_namespace(&second_directory, &second, b"{\"seq\":2}\n");
    let second_record = service
        .table
        .admitted_namespaces
        .lock()
        .expect("admitted registry")
        .get(second.handle_id.as_str())
        .cloned()
        .expect("second record");
    let second_bytes = b"{\"seq\":2}\n";
    let second_content_hash = hash_bytes(second_bytes);
    let second_frontier = PhysicalStorageFrontierV1 {
        byte_length: second_bytes.len() as u64,
        record_count: 1,
        content_hash: second_content_hash,
        frontier_hash: service
            .physical_frontier_hash(
                &second_record,
                second_bytes.len() as u64,
                1,
                second_content_hash,
            )
            .expect("second frontier hash"),
    };
    // Model a process stop after the physical observation became durable but before the
    // terminal settlement append. An older settlement for this grant must not mask it.
    service
        .ensure_physical_frontier(&second_record, second_frontier)
        .expect("persist second frontier");
    drop(service);

    let mut reopened_table = AuthorityStorageGrantTableV1::new();
    reopened_table.register(grant.clone()).expect("grant");
    let reopened = AuthorityManagedStorageServiceV1::new_with_journal(
        reopened_table,
        grant.authority_generation,
        &path,
        header.bootstrap_manifest_hash,
        header.journal_instance_hash,
    )
    .expect("reopen with exact pending admission");
    let receipts = reopened
        .reconcile_unsettled_storage_grants_with_physical_bridge()
        .expect("recover second admission");
    assert_eq!(receipts.len(), 1);
    assert_eq!(
        receipts[0].physical_frontier_hash,
        Some(second_frontier.frontier_hash)
    );
    reopened
        .require_startup_reconciliation()
        .expect("exact blocker cleared");

    let journal =
        crate::journal::ResourceJournalFileV1::open(&path, header).expect("reopen durable journal");
    assert!(journal.unsettled_storage_namespaces().is_empty());
    assert_eq!(
        journal
            .storage_recovery_records_for_admission(grant.grant_hash, second.namespace_hash)
            .len(),
        7
    );
}

fn write_physical_test_namespace(
    directory: &Path,
    handle: &ManagedStorageNamespaceHandleV1,
    records: &[u8],
) {
    std::fs::create_dir_all(directory).expect("managed namespace");
    std::fs::write(
        directory.join("authority-admission.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "handle_id": handle.handle_id.as_str(),
            "namespace_hash": handle.namespace_hash,
        }))
        .expect("marker json"),
    )
    .expect("marker");
    std::fs::write(directory.join("records.jsonl"), records).expect("records");
}

#[test]
fn r71_storage_quota_journal_reserves_reconciles_and_releases_owner() {
    use sigil_kernel::capability_issuer::KernelCapabilityBrokerV1;

    let directory = tempfile::tempdir().expect("journal directory");
    let path = directory.path().join("authority-resources.journal.json");
    let header = storage_test_header();
    let grant = grant();
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant.clone()).expect("grant");
    let service = AuthorityManagedStorageServiceV1::new_with_journal(
        table,
        grant.authority_generation,
        &path,
        header.bootstrap_manifest_hash,
        header.journal_instance_hash,
    )
    .expect("service");
    let broker = KernelCapabilityBrokerV1::new();
    let capability = broker
        .issue_storage_namespace_capability(
            broker.seal_storage_namespace_proof(grant.capability_family, grant.namespace_hash),
        )
        .expect("capability");
    let handle = service
        .admit_namespace(storage_test_request(), capability)
        .expect("admit");

    let namespace = directory.path().join("managed/session-log");
    std::fs::create_dir_all(&namespace).expect("managed root");
    std::fs::write(
        namespace.join("authority-admission.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "handle_id": handle.handle_id.as_str(),
            "namespace_hash": handle.namespace_hash,
        }))
        .expect("marker json"),
    )
    .expect("marker");
    let bytes = b"{}\n";
    std::fs::write(namespace.join("records.jsonl"), bytes).expect("records");
    let receipt = service
        .finalize_namespace_with_physical_frontier(
            handle,
            bytes.len() as u64,
            1,
            hash_bytes(bytes),
            "quota-test".to_owned(),
        )
        .expect("settle");
    assert!(receipt.physical_frontier_hash.is_some());

    let quota_bytes = std::fs::read(
        directory
            .path()
            .join(".authority-quota")
            .join("managed-storage.json"),
    )
    .expect("quota journal");
    let quota_text = String::from_utf8(quota_bytes).expect("quota json");
    assert!(quota_text.contains("storage:"));
    assert!(quota_text.contains("Released"));
}

#[test]
fn r71_storage_restart_releases_quota_owner_without_durable_admission() {
    let directory = tempfile::tempdir().expect("journal directory");
    let path = directory.path().join("authority-resources.journal.json");
    let quota_path = directory
        .path()
        .join(".authority-quota/managed-storage.json");
    let header = storage_test_header();
    let grant = grant();
    let workspace_cap = grant.quota_profile.max_bytes;
    {
        let mut quota = QuotaBookV1::open(&quota_path, workspace_cap).expect("quota journal");
        quota
            .reserve_owned("storage:orphan", &grant.quota_profile, 0, 1)
            .expect("orphan reservation");
    }

    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant.clone()).expect("grant");
    drop(
        AuthorityManagedStorageServiceV1::new_with_journal(
            table,
            grant.authority_generation,
            &path,
            header.bootstrap_manifest_hash,
            header.journal_instance_hash,
        )
        .expect("restart reconciliation"),
    );

    let reopened = QuotaBookV1::open(quota_path, workspace_cap).expect("reopen quota");
    assert!(reopened.reservation_for_owner("storage:orphan").is_none());
    assert_eq!(reopened.workspace_used_bytes(), 0);
}

#[test]
fn r71_storage_restart_keeps_quota_for_pending_legacy_owner_after_older_terminal() {
    let directory = tempfile::tempdir().expect("journal directory");
    let path = directory.path().join("authority-resources.journal.json");
    let header = storage_test_header();
    let grant = grant();
    let namespace_hash = CanonicalHash::from_bytes([3u8; 32]);
    {
        let mut journal =
            crate::journal::ResourceJournalFileV1::open(&path, header.clone()).expect("journal");
        for handle_id in ["settled-proof-4", "pending-proof-4"] {
            journal
                .append_event(
                    crate::journal::ResourceJournalEventV1::StorageNamespaceAdmitted {
                        grant_hash: grant.grant_hash,
                        handle_id: handle_id.to_owned(),
                        namespace_hash,
                        grant: Box::new(grant.clone()),
                        request: Box::new(storage_test_request()),
                    },
                )
                .expect("admission");
            if handle_id == "settled-proof-4" {
                journal
                    .append_event(crate::journal::ResourceJournalEventV1::GenerationSettled {
                        grant_hash: grant.grant_hash,
                        resource_id: grant.resource_ref.resource_id.as_str().to_owned(),
                        generation: grant.resource_ref.generation,
                        cleanup_status: "settled legacy admission".to_owned(),
                        physical_frontier_hash: None,
                        physical_observation_record_hash: None,
                    })
                    .expect("settlement");
            }
        }
    }

    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant.clone()).expect("grant");
    let service = AuthorityManagedStorageServiceV1::new_with_journal(
        table,
        grant.authority_generation,
        &path,
        header.bootstrap_manifest_hash,
        header.journal_instance_hash,
    )
    .expect("restart reconciliation");
    let owner_key = storage_quota_owner_key(grant.grant_hash, namespace_hash);
    assert!(
        service
            .quota
            .lock()
            .expect("quota")
            .reservation_for_owner(&owner_key)
            .is_some(),
        "an older terminal must not release the exact pending admission's legacy owner"
    );
    assert!(matches!(
        service.require_startup_reconciliation(),
        Err(ManagedStorageErrorV1::JournalUnavailable)
    ));
}

#[test]
fn r71_storage_physical_bridge_rejects_pending_without_marker() {
    let directory = tempfile::tempdir().expect("journal directory");
    let path = directory.path().join("authority-resources.journal.json");
    let header = storage_test_header();
    let grant = grant();
    {
        let mut journal =
            crate::journal::ResourceJournalFileV1::open(&path, header.clone()).expect("journal");
        journal
            .append_event(
                crate::journal::ResourceJournalEventV1::StorageNamespaceAdmitted {
                    grant_hash: grant.grant_hash,
                    handle_id: "missing-marker".to_owned(),
                    namespace_hash: CanonicalHash::from_bytes([3u8; 32]),
                    grant: Box::new(grant.clone()),
                    request: Box::new(storage_test_request()),
                },
            )
            .expect("admission");
    }
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant.clone()).expect("grant");
    let service = AuthorityManagedStorageServiceV1::new_with_journal(
        table,
        grant.authority_generation,
        &path,
        header.bootstrap_manifest_hash,
        header.journal_instance_hash,
    )
    .expect("rehydrate");
    assert!(matches!(
        service.reconcile_unsettled_storage_grants_with_physical_bridge(),
        Err(ManagedStorageErrorV1::JournalUnavailable)
    ));
}

#[test]
fn r71_storage_finalize_preserves_admission_when_settlement_persist_fails() {
    let directory = tempfile::tempdir().expect("journal directory");
    let path = directory.path().join("authority-resources.journal.json");
    let header = storage_test_header();
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant()).expect("grant");
    let service = AuthorityManagedStorageServiceV1::new_with_journal(
        table,
        grant().authority_generation,
        &path,
        header.bootstrap_manifest_hash,
        header.journal_instance_hash,
    )
    .expect("service");
    let handle = service
        .admit_namespace(
            storage_test_request(),
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        )
        .expect("admit");
    let original_path = path.clone();
    service
        .journal
        .as_ref()
        .expect("journal")
        .lock()
        .expect("journal lock")
        .set_path_for_test(directory.path().join("missing-parent").join("journal.json"));
    assert!(matches!(
        service.finalize_namespace(handle, "failed".to_owned()),
        Err(ManagedStorageErrorV1::JournalUnavailable)
    ));
    service
        .journal
        .as_ref()
        .expect("journal")
        .lock()
        .expect("journal lock")
        .set_path_for_test(original_path);
    let record = service
        .table
        .admitted_namespaces
        .lock()
        .expect("admitted namespaces")
        .get("handle-probe-storage-1")
        .cloned()
        .expect("retained admission");
    let downgraded = ManagedStorageNamespaceHandleV1::new(
        sigil_kernel::resource::OpaqueKernelCapabilityHandleId::new(record.handle_id.clone()),
        record.namespace_hash,
        record.grant.capability_family,
        OpaqueKernelCapabilityAuthenticatorV1::new("downgraded".to_owned()),
    );
    assert!(matches!(
        service.validate_namespace_write(&downgraded),
        Err(ManagedStorageErrorV1::CapabilityMismatch)
    ));
    let receipt = service
        .finalize_namespace(
            recovery_namespace_handle(record.handle_id.clone(), &record),
            "recovered".to_owned(),
        )
        .expect("retry finalize");
    assert!(receipt.committed_sequence_or_version.is_some());
}

#[test]
fn r71_storage_family_exact_closure() {
    use sigil_kernel::managed_storage::{
        ManagedStorageAdmissionRequestV1, ValidatedStorageAdmissionCapabilityV1,
    };
    use sigil_kernel::resource::{
        ManagedStorageCapabilityFamilyV1, ManagedStorageSemanticOwnerV1, OpaqueSessionId,
        ResourceJournalScopeV1, ResourceOwnerScopeV1,
    };
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant()).expect("register");
    let service = AuthorityManagedStorageServiceV1::new(
        table,
        AuthorityGeneration {
            epoch: 1,
            instance_hash: CanonicalHash::from_bytes([8u8; 32]),
        },
    );
    let exact = ManagedStorageAdmissionRequestV1 {
        semantic_owner: ManagedStorageSemanticOwnerV1::SessionLog,
        capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
        purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
        source: sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
            cutover_manifest_hash: CanonicalHash::from_bytes([9u8; 32]),
            application_generation: 1,
        },
        owner_scope: ResourceOwnerScopeV1::Session(OpaqueSessionId::new("s-1".to_owned())),
        journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
    };
    service
        .admit_namespace(
            exact,
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        )
        .expect("exact family+owner");
    // Same family but different semantic owner: refused (a different writer must not piggyback).
    let piggyback = ManagedStorageAdmissionRequestV1 {
        semantic_owner: ManagedStorageSemanticOwnerV1::InteractiveInputHistory,
        capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
        purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
        source: sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
            cutover_manifest_hash: CanonicalHash::from_bytes([9u8; 32]),
            application_generation: 1,
        },
        owner_scope: ResourceOwnerScopeV1::Session(OpaqueSessionId::new("s-1".to_owned())),
        journal_scope: ResourceJournalScopeV1::Application,
    };
    let error = service
        .admit_namespace(
            piggyback,
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        )
        .expect_err("piggyback");
    assert!(matches!(error, ManagedStorageErrorV1::FamilyMismatch));
    // Unrelated family: refused, never a masqueraded readiness.
    let unrelated = ManagedStorageAdmissionRequestV1 {
        semantic_owner: ManagedStorageSemanticOwnerV1::SessionCatalog,
        capability_family: ManagedStorageCapabilityFamilyV1::JournaledAtomicProjection,
        purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::RebuildableProjection,
        source: sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
            cutover_manifest_hash: CanonicalHash::from_bytes([9u8; 32]),
            application_generation: 1,
        },
        owner_scope: ResourceOwnerScopeV1::Session(OpaqueSessionId::new("s-1".to_owned())),
        journal_scope: ResourceJournalScopeV1::Application,
    };
    let error = service
        .admit_namespace(
            unrelated,
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        )
        .expect_err("unrelated");
    assert!(matches!(error, ManagedStorageErrorV1::FamilyMismatch));
}

#[test]
fn r71_storage_broker_binding_rejects_namespace_and_family_drift() {
    use sigil_kernel::capability_issuer::KernelCapabilityBrokerV1;
    use sigil_kernel::managed_storage::StorageAdmissionSourceV1;
    use sigil_kernel::resource::{ManagedStorageCapabilityFamilyV1, ManagedStorageSemanticOwnerV1};
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant()).expect("register");
    let service = AuthorityManagedStorageServiceV1::new(
        table,
        AuthorityGeneration {
            epoch: 1,
            instance_hash: CanonicalHash::from_bytes([8u8; 32]),
        },
    );
    let request = ManagedStorageAdmissionRequestV1 {
        semantic_owner: ManagedStorageSemanticOwnerV1::SessionLog,
        capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
        purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
        source: StorageAdmissionSourceV1::ApplicationCutoverRoot {
            cutover_manifest_hash: CanonicalHash::from_bytes([9u8; 32]),
            application_generation: 1,
        },
        owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
        journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
    };
    let broker = KernelCapabilityBrokerV1::new();
    let exact = broker
        .issue_storage_namespace_capability(broker.seal_storage_namespace_proof(
            ManagedStorageCapabilityFamilyV1::AppendLog,
            CanonicalHash::from_bytes([3u8; 32]),
        ))
        .expect("exact capability");
    service
        .admit_namespace(request.clone(), exact)
        .expect("exact binding");
    let wrong_namespace = broker
        .issue_storage_namespace_capability(broker.seal_storage_namespace_proof(
            ManagedStorageCapabilityFamilyV1::AppendLog,
            CanonicalHash::from_bytes([4u8; 32]),
        ))
        .expect("wrong namespace capability");
    let error = service
        .admit_namespace(request.clone(), wrong_namespace)
        .expect_err("namespace drift");
    assert!(matches!(error, ManagedStorageErrorV1::CapabilityMismatch));
    let wrong_family = broker
        .issue_storage_namespace_capability(broker.seal_storage_namespace_proof(
            ManagedStorageCapabilityFamilyV1::AtomicObject,
            CanonicalHash::from_bytes([3u8; 32]),
        ))
        .expect("wrong family capability");
    let error = service
        .admit_namespace(request, wrong_family)
        .expect_err("family drift");
    assert!(matches!(error, ManagedStorageErrorV1::CapabilityMismatch));
}
