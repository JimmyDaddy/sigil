use super::*;

fn header() -> ResourceJournalHeaderV1 {
    let instance = journal_encode(b"instance-1");
    ResourceJournalHeaderV1 {
        schema_version: 1,
        shard_name: "application-resources".to_owned(),
        bootstrap_manifest_hash: journal_encode(b"manifest-1"),
        journal_instance_hash: instance,
        header_hash: journal_encode(b"header-1"),
    }
}

#[test]
fn r71_journal_first_record_must_be_bootstrap_bound() {
    let mut journal = ResourceJournalMemoryV1::new();
    journal.install_header(header()).expect("header");
    let precondition = ResourceJournalAppendPreconditionV1::Empty {
        expected_header_hash: journal.header().expect("h").header_hash,
        expected_journal_instance_hash: journal.header().expect("h").journal_instance_hash,
    };
    let error = journal
        .append(
            &precondition,
            &ResourceJournalEventV1::GenerationReserved {
                resource_id: "r".to_owned(),
                generation: 1,
            },
        )
        .expect_err("must reject non-bootstrap first record");
    assert!(matches!(
        error,
        JournalErrorV1::FirstRecordNotBootstrapBound
    ));
}

#[test]
fn r71_journal_genesis_sequence_is_one_and_chain_is_unique() {
    let mut journal = ResourceJournalMemoryV1::new();
    journal.install_header(header()).expect("header");
    let h = journal.header().expect("h").clone();
    let first = journal
        .append(
            &ResourceJournalAppendPreconditionV1::Empty {
                expected_header_hash: h.header_hash,
                expected_journal_instance_hash: h.journal_instance_hash,
            },
            &ResourceJournalEventV1::BootstrapBound {
                bootstrap_manifest_hash: h.bootstrap_manifest_hash,
            },
        )
        .expect("genesis");
    assert_eq!(first.sequence, 1);
    // Duplicate genesis precondition fails.
    let error = journal
        .append(
            &ResourceJournalAppendPreconditionV1::Empty {
                expected_header_hash: h.header_hash,
                expected_journal_instance_hash: h.journal_instance_hash,
            },
            &ResourceJournalEventV1::BootstrapBound {
                bootstrap_manifest_hash: h.bootstrap_manifest_hash,
            },
        )
        .expect_err("duplicate genesis must fail");
    assert!(matches!(error, JournalErrorV1::PreconditionMismatch));
}

#[test]
fn r71_journal_chain_must_match_exact_tail() {
    let mut journal = ResourceJournalMemoryV1::new();
    journal.install_header(header()).expect("header");
    let h = journal.header().expect("h").clone();
    journal
        .append(
            &ResourceJournalAppendPreconditionV1::Empty {
                expected_header_hash: h.header_hash,
                expected_journal_instance_hash: h.journal_instance_hash,
            },
            &ResourceJournalEventV1::BootstrapBound {
                bootstrap_manifest_hash: h.bootstrap_manifest_hash,
            },
        )
        .expect("genesis");
    // Wrong expected tail is rejected.
    let error = journal
        .append(
            &ResourceJournalAppendPreconditionV1::Existing {
                expected_sequence: 999,
                expected_record_hash: journal_encode(b"wrong"),
                expected_journal_instance_hash: h.journal_instance_hash,
            },
            &ResourceJournalEventV1::GenerationReserved {
                resource_id: "r".to_owned(),
                generation: 1,
            },
        )
        .expect_err("wrong precondition must fail");
    assert!(matches!(error, JournalErrorV1::PreconditionMismatch));
}

#[test]
fn r71_durable_journal_replays_after_process_restart_and_rejects_corruption() {
    let directory = tempfile::tempdir().expect("journal directory");
    let path = directory.path().join("authority.journal.json");
    let h = header();
    let grant_hash = journal_encode(b"grant-1");
    {
        let mut journal = ResourceJournalFileV1::open(&path, h.clone()).expect("create");
        let record = journal
            .append_event(ResourceJournalEventV1::StorageNamespaceAdmitted {
                grant_hash,
                handle_id: "handle-1".to_owned(),
                namespace_hash: journal_encode(b"namespace-1"),
                grant: Box::new(sample_grant()),
                request: Box::new(sample_request()),
            })
            .expect("append admission");
        assert_eq!(record.sequence, 2, "genesis is persisted before admissions");
    }
    let reopened = ResourceJournalFileV1::open(&path, h.clone()).expect("replay");
    assert_eq!(reopened.tail().expect("tail").sequence, 2);
    assert_eq!(reopened.header().expect("header"), &h);
    assert!(
        reopened
            .unsettled_storage_grants()
            .contains(&grant_hash.to_hex())
    );

    std::fs::write(&path, b"truncated").expect("corrupt journal");
    let error = ResourceJournalFileV1::open(&path, h).expect_err("corruption fails closed");
    assert!(matches!(error, JournalErrorV1::Corrupt(_)));
}

fn sample_grant() -> StorageAdmissionGrantV1 {
    StorageAdmissionGrantV1 {
        grant_id: sigil_kernel::resource::OpaqueStorageGrantId::new("journal-grant".to_owned()),
        admission_hash: journal_encode(b"admission"),
        semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLog,
        purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
        purpose_hash: journal_encode(b"purpose"),
        source_class: sigil_kernel::resource::StorageAdmissionSourceClassV1::ApplicationCutoverRoot,
        source_binding_hash: journal_encode(b"source"),
        namespace_hash: journal_encode(b"namespace-1"),
        journal_scope: ResourceJournalScopeV1::Application,
        journal_scope_hash: journal_encode(b"scope"),
        resource_ref: sigil_kernel::resource::ResourceRefV1 {
            resource_id: sigil_kernel::resource::OpaqueResourceId::new("resource".to_owned()),
            kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
            owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
            journal_scope: ResourceJournalScopeV1::Application,
            generation: 1,
        },
        resource_binding_digest: journal_encode(b"resource-binding"),
        physical_binding_hash: journal_encode(b"physical"),
        resource_kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
        owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
        capability_family: sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog,
        retention_policy: sigil_kernel::resource::ResourceRetentionPolicyV1::SessionPolicy,
        quota_profile: sigil_kernel::resource::ResourceQuotaProfileV1 {
            class: sigil_kernel::resource::ResourceQuotaClassV1::RuntimeState,
            max_bytes: 1024,
            max_entries: 8,
            max_open_holders: 1,
            max_age_ms: None,
            hard_runtime_enforcement_required: true,
            profile_hash: journal_encode(b"quota"),
        },
        semantic_schema: sigil_kernel::resource::OpaqueSemanticSchemaId::new("schema".to_owned()),
        authority_generation: sigil_kernel::resource::AuthorityGeneration {
            epoch: 1,
            instance_hash: journal_encode(b"authority"),
        },
        journal_admission_sequence: 1,
        grant_hash: journal_encode(b"grant-hash"),
    }
}

fn sample_request() -> ManagedStorageAdmissionRequestV1 {
    ManagedStorageAdmissionRequestV1 {
        semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLog,
        capability_family: sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog,
        purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
        source: sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
            cutover_manifest_hash: journal_encode(b"source"),
            application_generation: 1,
        },
        owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
        journal_scope: ResourceJournalScopeV1::Application,
    }
}

#[test]
fn r71_durable_journal_rolls_back_memory_after_pre_rename_persist_failure() {
    let directory = tempfile::tempdir().expect("journal directory");
    let path = directory.path().join("authority.journal.json");
    let mut journal = ResourceJournalFileV1::open(path.clone(), header()).expect("create");
    journal.path = directory.path().join("missing-parent").join("journal.json");
    let error = journal
        .append_event(ResourceJournalEventV1::GenerationReserved {
            resource_id: "resource".to_owned(),
            generation: 2,
        })
        .expect_err("persist failure");
    assert!(matches!(error, JournalErrorV1::Filesystem(_)));
    assert_eq!(journal.tail().expect("genesis").sequence, 1);
    assert_eq!(journal.records.len(), 1);

    journal.path = path.clone();
    journal
        .append_event(ResourceJournalEventV1::GenerationReserved {
            resource_id: "resource".to_owned(),
            generation: 3,
        })
        .expect("retry after rollback");
    let reopened = ResourceJournalFileV1::open(path, header()).expect("reopen");
    assert_eq!(reopened.tail().expect("tail").sequence, 2);
}

#[test]
fn r71_durable_journal_rejects_stale_cross_process_snapshot_without_lost_update() {
    let directory = tempfile::tempdir().expect("journal directory");
    let path = directory.path().join("authority.journal.json");
    let mut first = ResourceJournalFileV1::open(&path, header()).expect("first writer");
    let mut stale = ResourceJournalFileV1::open(&path, header()).expect("stale writer");

    first
        .append_event(ResourceJournalEventV1::GenerationReserved {
            resource_id: "first".to_owned(),
            generation: 2,
        })
        .expect("first append");
    let error = stale
        .append_event(ResourceJournalEventV1::GenerationReserved {
            resource_id: "stale".to_owned(),
            generation: 3,
        })
        .expect_err("stale snapshot must not replace the first append");
    assert!(matches!(error, JournalErrorV1::PreconditionMismatch));
    assert_eq!(stale.tail().expect("rolled back stale tail").sequence, 1);

    let reopened = ResourceJournalFileV1::open(path, header()).expect("reopen");
    assert_eq!(reopened.tail().expect("first append retained").sequence, 2);
    assert_eq!(
        reopened.records.last().map(|durable| &durable.payload),
        Some(&ResourceJournalEventV1::GenerationReserved {
            resource_id: "first".to_owned(),
            generation: 2,
        })
    );
}
