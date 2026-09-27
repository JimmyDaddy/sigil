use super::*;
use sigil_kernel::managed_storage::{ManagedStorageServiceV1, StorageAdmissionSourceV1};
use sigil_kernel::resource::{
    ManagedStorageAdmissionPurposeV1, ManagedStorageCapabilityFamilyV1, ResourceAuthorityScopeV1,
    ResourceKindV1, ResourceOwnerScopeV1, ResourceQuotaClassV1, ResourceRefV1,
    ResourceRetentionPolicyV1, StorageAdmissionSourceClassV1,
};

pub(super) fn source() -> StorageAdmissionSourceV1 {
    StorageAdmissionSourceV1::ApplicationCutoverRoot {
        cutover_manifest_hash: CanonicalHash::from_bytes([0x11; 32]),
        application_generation: 1,
    }
}

pub(super) fn authority() -> AuthorityGeneration {
    AuthorityGeneration {
        epoch: 1,
        instance_hash: CanonicalHash::from_bytes([0x12; 32]),
    }
}

pub(super) fn grant() -> StorageAdmissionGrantV1 {
    StorageAdmissionGrantV1 {
        grant_id: OpaqueStorageGrantId::new("grant-storage-test".to_owned()),
        admission_hash: CanonicalHash::from_bytes([1; 32]),
        semantic_owner: ManagedStorageSemanticOwnerV1::SessionLog,
        purpose: ManagedStorageAdmissionPurposeV1::DurablePayload,
        purpose_hash: CanonicalHash::from_bytes([2; 32]),
        source_class: StorageAdmissionSourceClassV1::ApplicationCutoverRoot,
        source_binding_hash: admission_source_binding_hash(&source()),
        namespace_hash: CanonicalHash::from_bytes([3; 32]),
        authority_scope: ResourceAuthorityScopeV1::Application,
        authority_scope_hash: CanonicalHash::from_bytes([4; 32]),
        resource_ref: ResourceRefV1 {
            resource_id: sigil_kernel::resource::OpaqueResourceId::new(
                "resource-storage-test".to_owned(),
            ),
            kind: ResourceKindV1::RuntimeState,
            owner_scope: ResourceOwnerScopeV1::Application,
            authority_scope: ResourceAuthorityScopeV1::Application,
            generation: 1,
        },
        resource_binding_digest: CanonicalHash::from_bytes([5; 32]),
        physical_binding_hash: CanonicalHash::from_bytes([6; 32]),
        resource_kind: ResourceKindV1::RuntimeState,
        owner_scope: ResourceOwnerScopeV1::Application,
        capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
        retention_policy: ResourceRetentionPolicyV1::SessionPolicy,
        quota_profile: sigil_kernel::resource::ResourceQuotaProfileV1 {
            class: ResourceQuotaClassV1::RuntimeState,
            max_bytes: 1024,
            max_entries: 100,
            max_open_holders: 1,
            max_age_ms: None,
            hard_runtime_enforcement_required: true,
            profile_hash: CanonicalHash::from_bytes([7; 32]),
        },
        semantic_schema: sigil_kernel::resource::OpaqueSemanticSchemaId::new(
            "schema-storage-test".to_owned(),
        ),
        authority_generation: authority(),
        grant_hash: CanonicalHash::from_bytes([8; 32]),
    }
}

fn request(namespace_key_hash: CanonicalHash) -> ManagedStorageAdmissionRequestV1 {
    ManagedStorageAdmissionRequestV1 {
        semantic_owner: ManagedStorageSemanticOwnerV1::SessionLog,
        capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
        purpose: ManagedStorageAdmissionPurposeV1::DurablePayload,
        source: source(),
        owner_scope: ResourceOwnerScopeV1::Application,
        authority_scope: ResourceAuthorityScopeV1::Application,
        namespace_key_hash,
    }
}

#[test]
fn current_grant_table_rejects_duplicate_grants() {
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant()).expect("first registration");
    assert!(matches!(
        table.register(grant()),
        Err(ManagedStorageErrorV1::CapabilityMismatch)
    ));
}

#[test]
fn logical_key_registry_rejects_duplicate_key_ids() {
    let mut registry = AuthorityLogicalKeyRegistryV1::default();
    let key = OpaqueStorageKeyIdV1::new("key-storage-test".to_owned());
    registry
        .reserve(
            key.clone(),
            sigil_kernel::resource::StorageLogicalKeyKindV1::Object,
        )
        .expect("first key");
    assert!(matches!(
        registry.reserve(key, sigil_kernel::resource::StorageLogicalKeyKindV1::Stream),
        Err(ManagedStorageErrorV1::DuplicateClaim)
    ));
}

#[test]
fn admission_is_current_state_only_and_settles_once() {
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant()).expect("grant");
    let service = AuthorityManagedStorageServiceV1::new(table, authority());
    let namespace = CanonicalHash::from_bytes([0x55; 32]);
    let handle = service
        .admit_namespace(
            request(namespace),
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        )
        .expect("admit");
    service
        .validate_namespace_write(&handle)
        .expect("live lease");
    let receipt = service
        .finalize_namespace_with_physical_frontier(
            handle,
            12,
            1,
            CanonicalHash::from_bytes([0x56; 32]),
            "test-settlement".to_owned(),
        )
        .expect("settle");
    assert_eq!(receipt.committed_entry_count, Some(1));
    assert!(matches!(
        service.validate_namespace_write(&ManagedStorageNamespaceHandleV1::new(
            OpaqueKernelCapabilityHandleId::new("handle-probe-storage-2".to_owned()),
            namespace,
            ManagedStorageCapabilityFamilyV1::AppendLog,
            OpaqueKernelCapabilityAuthenticatorV1::new("test".to_owned()),
        )),
        Err(ManagedStorageErrorV1::HandleFinalized)
    ));
}

#[test]
fn sample_receipt_keeps_physical_observation_optional() {
    let receipt = sample_storage_receipt();
    assert_eq!(
        receipt.semantic_owner,
        ManagedStorageSemanticOwnerV1::SessionLifecycleLog
    );
    assert!(receipt.physical_frontier_hash.is_none());
}

fn table_with_session_grant() -> AuthorityStorageGrantTableV1 {
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant()).expect("session grant");
    table
}

fn quota_journal_path(root: &Path) -> PathBuf {
    root.join(".authority-quota").join("managed-storage.json")
}

#[test]
fn storage_capacity_grant_checks_live_handle_generation_and_typed_limits() {
    let directory = tempfile::tempdir().expect("storage root");
    let mut service = AuthorityManagedStorageServiceV1::new_with_state_root(
        table_with_session_grant(),
        authority(),
        directory.path(),
    )
    .expect("durable authority");
    let namespace = CanonicalHash::from_bytes([0x71; 32]);
    let handle = service
        .admit_namespace(
            request(namespace),
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        )
        .expect("handle");
    assert_eq!(
        service
            .reserve_namespace_quota_capacity(&handle, 10, 100, 1)
            .expect("capacity"),
        100
    );
    let before = fs::read(quota_journal_path(directory.path())).expect("before rejection");
    assert!(matches!(
        service.reserve_namespace_quota_capacity(&handle, 1025, 2048, 1),
        Err(ManagedStorageErrorV1::QuotaExceeded {
            dimension: sigil_kernel::managed_storage::ManagedStorageQuotaDimensionV1::Bytes,
            requested: 1025,
            limit: 1024,
        })
    ));
    assert!(matches!(
        service.reserve_namespace_quota_capacity(&handle, 100, 100, 101),
        Err(ManagedStorageErrorV1::QuotaExceeded {
            dimension: sigil_kernel::managed_storage::ManagedStorageQuotaDimensionV1::Entries,
            requested: 101,
            limit: 100,
        })
    ));
    let wrong_family = ManagedStorageNamespaceHandleV1::new(
        OpaqueKernelCapabilityHandleId::new(handle.handle_id.as_str().to_owned()),
        namespace,
        ManagedStorageCapabilityFamilyV1::AtomicObject,
        OpaqueKernelCapabilityAuthenticatorV1::new("test".to_owned()),
    );
    assert!(matches!(
        service.reserve_namespace_quota_capacity(&wrong_family, 100, 100, 1),
        Err(ManagedStorageErrorV1::CapabilityMismatch)
    ));
    service.authority_generation.epoch += 1;
    assert!(matches!(
        service.reserve_namespace_quota_capacity(&handle, 100, 100, 1),
        Err(ManagedStorageErrorV1::CapabilityMismatch)
    ));
    service.authority_generation = authority();
    assert_eq!(
        fs::read(quota_journal_path(directory.path())).expect("no quota change"),
        before
    );
    let stale = ManagedStorageNamespaceHandleV1::new(
        OpaqueKernelCapabilityHandleId::new(handle.handle_id.as_str().to_owned()),
        namespace,
        handle.capability_family,
        OpaqueKernelCapabilityAuthenticatorV1::new("test".to_owned()),
    );
    service
        .finalize_namespace(handle, "capacity-test".to_owned())
        .expect("finalize");
    assert!(matches!(
        service.reserve_namespace_quota_capacity(&stale, 1, 100, 1),
        Err(ManagedStorageErrorV1::HandleFinalized)
    ));
}

#[test]
fn storage_shared_quota_poison_rejects_writes_against_existing_capacity() {
    let directory = tempfile::tempdir().expect("storage root");
    let first = AuthorityManagedStorageServiceV1::new_with_state_root(
        table_with_session_grant(),
        authority(),
        directory.path(),
    )
    .expect("first service");
    let second = AuthorityManagedStorageServiceV1::new_with_state_root(
        table_with_session_grant(),
        authority(),
        directory.path(),
    )
    .expect("second service sharing the quota book");
    let first_handle = first
        .admit_namespace(
            request(CanonicalHash::from_bytes([0x72; 32])),
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        )
        .expect("first namespace");
    let second_handle = second
        .admit_namespace(
            request(CanonicalHash::from_bytes([0x73; 32])),
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        )
        .expect("second namespace");
    assert_eq!(
        first
            .reserve_namespace_quota_capacity(&first_handle, 4, 100, 1)
            .expect("existing capacity"),
        100
    );
    first
        .validate_namespace_write(&first_handle)
        .expect("healthy capacity can be consumed");
    let first_record = first
        .record_for_handle(&first_handle)
        .expect("live first record");
    let first_owner =
        storage_quota_owner_key(first_record.grant.grant_hash, first_record.namespace_hash);
    let previous = first
        .quota
        .lock()
        .expect("quota")
        .reservation_for_owner(&first_owner)
        .expect("first reservation");
    second
        .quota
        .lock()
        .expect("shared quota")
        .inject_next_directory_sync_failure();
    assert!(matches!(
        second.reserve_namespace_quota_capacity(&second_handle, 4, 100, 1),
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    ));
    let after_uncertain_write =
        fs::read(quota_journal_path(directory.path())).expect("installed snapshot");
    assert!(
        first.record_for_handle(&first_handle).is_ok(),
        "the namespace remains live"
    );
    assert_eq!(
        first
            .quota
            .lock()
            .expect("quota")
            .reservation_for_owner(&first_owner),
        Some(previous),
        "the previous capacity is still charged"
    );
    for _ in 0..10 {
        assert!(
            matches!(
                first.validate_namespace_write(&first_handle),
                Err(ManagedStorageErrorV1::AuthorityUnavailable)
            ),
            "a live namespace cannot consume capacity from a poisoned shared book"
        );
    }
    assert_eq!(
        fs::read(quota_journal_path(directory.path())).expect("validation snapshot"),
        after_uncertain_write,
        "validation must not write the journal"
    );
}

#[test]
fn storage_workspace_policy_cold_migration_preserves_settled_records() {
    let directory = tempfile::tempdir().expect("storage root");
    let journal = quota_journal_path(directory.path());
    let service = AuthorityManagedStorageServiceV1::new_with_state_root(
        table_with_session_grant(),
        authority(),
        directory.path(),
    )
    .expect("original computed cap");
    let handle = service
        .admit_namespace(
            request(CanonicalHash::from_bytes([0x61; 32])),
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        )
        .expect("old admission");
    service
        .reconcile_namespace_quota(&handle, 40, 1)
        .expect("old reservation");
    service
        .finalize_namespace(handle, "settled-before-upgrade".to_owned())
        .expect("old settlement");
    drop(service);
    let mut expected: serde_json::Value =
        serde_json::from_slice(&fs::read(&journal).expect("original journal"))
            .expect("original snapshot");
    assert_eq!(expected["workspace_cap"], 1024);
    assert!(!expected["records"].as_array().expect("records").is_empty());
    expected["workspace_cap"] = 2048.into();

    let migrated = AuthorityManagedStorageServiceV1::new_with_state_root_and_workspace_cap(
        table_with_session_grant(),
        authority(),
        directory.path(),
        2048,
        Some(1024),
    )
    .expect("exact policy migration");
    let snapshot: serde_json::Value =
        serde_json::from_slice(&fs::read(&journal).expect("migrated journal"))
            .expect("migrated snapshot");
    assert_eq!(
        snapshot, expected,
        "migration retains every historical record"
    );
    assert_eq!(migrated.quota.lock().expect("quota").workspace_cap(), 2048);
    drop(migrated);
    AuthorityManagedStorageServiceV1::new_with_state_root_and_workspace_cap(
        table_with_session_grant(),
        authority(),
        directory.path(),
        2048,
        None,
    )
    .expect("strict reopen after migration");
}

#[test]
fn storage_workspace_policy_cold_unknown_cap_is_rejected_without_journal_changes() {
    let directory = tempfile::tempdir().expect("storage root");
    drop(
        AuthorityManagedStorageServiceV1::new_with_state_root(
            table_with_session_grant(),
            authority(),
            directory.path(),
        )
        .expect("original cap"),
    );
    let journal = quota_journal_path(directory.path());
    let before = fs::read(&journal).expect("original journal");
    for (workspace_cap, previous_cap) in [(2048, None), (2048, Some(1023)), (512, Some(1024))] {
        let result = AuthorityManagedStorageServiceV1::new_with_state_root_and_workspace_cap(
            table_with_session_grant(),
            authority(),
            directory.path(),
            workspace_cap,
            previous_cap,
        );
        assert!(matches!(result, Err(QuotaErrorV1::Journal(_))));
        assert_eq!(fs::read(&journal).expect("rejected journal"), before);
    }
}

#[test]
fn storage_workspace_policy_warm_table_change_preserves_active_leases() {
    let directory = tempfile::tempdir().expect("storage root");
    let mut full_table = table_with_session_grant();
    let mut optional_grant = grant();
    optional_grant.grant_id = OpaqueStorageGrantId::new("optional-lifecycle-grant".to_owned());
    optional_grant.semantic_owner = ManagedStorageSemanticOwnerV1::SessionLifecycleLog;
    optional_grant.grant_hash = CanonicalHash::from_bytes([0x62; 32]);
    full_table.register(optional_grant).expect("optional grant");
    let first = AuthorityManagedStorageServiceV1::new_with_state_root_and_workspace_cap(
        full_table,
        authority(),
        directory.path(),
        2048,
        None,
    )
    .expect("full table");
    let old_handle = first
        .admit_namespace(
            request(CanonicalHash::from_bytes([0x63; 32])),
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        )
        .expect("old live admission");
    first
        .reconcile_namespace_quota(&old_handle, 40, 1)
        .expect("old live usage");
    let journal = quota_journal_path(directory.path());
    let before = fs::read(&journal).expect("active journal");

    let inferred_policy = AuthorityManagedStorageServiceV1::new_with_state_root(
        table_with_session_grant(),
        authority(),
        directory.path(),
    );
    assert!(matches!(inferred_policy, Err(QuotaErrorV1::Journal(_))));
    let second = AuthorityManagedStorageServiceV1::new_with_state_root_and_workspace_cap(
        table_with_session_grant(),
        authority(),
        directory.path(),
        2048,
        Some(1024),
    )
    .expect("reduced table with unchanged workspace policy");
    assert!(Arc::ptr_eq(&first.quota, &second.quota));
    assert_eq!(fs::read(&journal).expect("shared journal"), before);
    assert_eq!(
        second.quota.lock().expect("quota").workspace_used_bytes(),
        40
    );
    first
        .validate_namespace_write(&old_handle)
        .expect("old lease remains live");
    let new_handle = second
        .admit_namespace(
            request(CanonicalHash::from_bytes([0x64; 32])),
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        )
        .expect("new live admission");
    second
        .reconcile_namespace_quota(&new_handle, 60, 1)
        .expect("new live usage");
    assert_eq!(
        first.quota.lock().expect("quota").workspace_used_bytes(),
        100
    );
    first
        .finalize_namespace(old_handle, "settle-original-lease".to_owned())
        .expect("original owner settles normally");
    assert_eq!(
        second.quota.lock().expect("quota").workspace_used_bytes(),
        60
    );
    second
        .finalize_namespace(new_handle, "settle-new-lease".to_owned())
        .expect("new owner settles normally");
    assert_eq!(first.quota.lock().expect("quota").workspace_used_bytes(), 0);
}

#[test]
fn storage_workspace_policy_warm_cap_change_rejects_without_disturbing_active_lease() {
    let directory = tempfile::tempdir().expect("storage root");
    let service = AuthorityManagedStorageServiceV1::new_with_state_root(
        table_with_session_grant(),
        authority(),
        directory.path(),
    )
    .expect("original cap");
    let handle = service
        .admit_namespace(
            request(CanonicalHash::from_bytes([0x65; 32])),
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        )
        .expect("live admission");
    service
        .reconcile_namespace_quota(&handle, 40, 1)
        .expect("live reservation");
    let journal = quota_journal_path(directory.path());
    let before = fs::read(&journal).expect("active journal");
    for (workspace_cap, previous_cap) in [(2048, None), (2048, Some(1024)), (512, Some(1024))] {
        let result = AuthorityManagedStorageServiceV1::new_with_state_root_and_workspace_cap(
            table_with_session_grant(),
            authority(),
            directory.path(),
            workspace_cap,
            previous_cap,
        );
        assert!(matches!(
            result,
            Err(QuotaErrorV1::Journal(message))
                if message == "active managed storage workspace cap mismatch"
        ));
        assert_eq!(
            fs::read(&journal).expect("unchanged active journal"),
            before
        );
        assert_eq!(
            service.quota.lock().expect("quota").workspace_used_bytes(),
            40
        );
        service
            .validate_namespace_write(&handle)
            .expect("rejected policy cannot invalidate old lease");
    }
    service
        .finalize_namespace(handle, "settle-after-rejected-policy".to_owned())
        .expect("original lease remains settleable");
    assert_eq!(
        service.quota.lock().expect("quota").workspace_used_bytes(),
        0
    );
}

#[test]
fn session_finalize_observation_failure_releases_only_consumed_holder_and_preserves_charge() {
    for failure in ["missing_lock", "incomplete_record", "frontier_mismatch"] {
        let root = tempfile::tempdir().expect("isolated authority root");
        let service = AuthorityManagedStorageServiceV1::new_with_state_root(
            table_with_session_grant(),
            authority(),
            root.path(),
        )
        .expect("authority");
        let namespace = CanonicalHash::from_bytes([0x81; 32]);
        let other_namespace = CanonicalHash::from_bytes([0x82; 32]);
        let handle = service
            .admit_namespace(
                request(namespace),
                ValidatedStorageAdmissionCapabilityV1::startup_probe(),
            )
            .expect("session holder");
        let other = service
            .admit_namespace(
                request(other_namespace),
                ValidatedStorageAdmissionCapabilityV1::startup_probe(),
            )
            .expect("unrelated holder");
        service
            .reconcile_namespace_quota(&handle, 64, 1)
            .expect("pending physical write charge");
        service
            .reconcile_namespace_quota(&other, 16, 1)
            .expect("unrelated charge");
        fs::create_dir_all(root.path().join("managed").join("session-log"))
            .expect("existing owner directory for no-follow physical resolution");
        let directory = service
            .physical_namespace_directory_for(ManagedStorageSemanticOwnerV1::SessionLog, namespace)
            .expect("admitted physical namespace");
        fs::create_dir_all(&directory).expect("fixture physical namespace");
        let complete = b"{\"published\":true}\n";
        let bytes: &[u8] = if failure == "incomplete_record" {
            b"{\"published\":"
        } else {
            complete
        };
        fs::write(directory.join("records.jsonl"), bytes).expect("actual pending record bytes");
        if failure != "missing_lock" {
            fs::write(directory.join(".authority-storage.lock"), b"").expect("namespace lock");
        }
        let before = fs::read(quota_journal_path(root.path())).expect("charged quota snapshot");
        let result = service.finalize_namespace_with_physical_frontier(
            handle,
            complete.len() as u64,
            1,
            if failure == "frontier_mismatch" {
                CanonicalHash::from_bytes([0x83; 32])
            } else {
                hash_bytes(complete)
            },
            "failed physical settlement".to_owned(),
        );
        assert!(
            matches!(result, Err(ManagedStorageErrorV1::AuthorityUnavailable)),
            "{failure}"
        );
        assert_eq!(
            fs::read(directory.join("records.jsonl")).expect("preserved recovery bytes"),
            bytes
        );
        assert_eq!(
            fs::read(quota_journal_path(root.path())).expect("pending quota snapshot"),
            before
        );
        assert_eq!(
            service.quota.lock().expect("quota").workspace_used_bytes(),
            80
        );
        service
            .validate_namespace_write(&other)
            .expect("unrelated holder remains live");

        let retry_authority = AuthorityManagedStorageServiceV1::new_with_state_root(
            table_with_session_grant(),
            authority(),
            root.path(),
        )
        .expect("another authority view of the same current resources");
        assert!(
            matches!(
                retry_authority.admit_namespace(
                    request(other_namespace),
                    ValidatedStorageAdmissionCapabilityV1::startup_probe(),
                ),
                Err(ManagedStorageErrorV1::DuplicateClaim)
            ),
            "unrelated live ownership cannot be stolen"
        );
        let retry = retry_authority
            .admit_namespace(
                request(namespace),
                ValidatedStorageAdmissionCapabilityV1::startup_probe(),
            )
            .expect("consumed failed holder permits explicit recovery with the same charge");
        assert_eq!(
            fs::read(quota_journal_path(root.path())).expect("reused charge"),
            before
        );
        fs::write(directory.join(".authority-storage.lock"), b"").expect("restore physical lock");
        fs::write(directory.join("records.jsonl"), complete)
            .expect("fixture completes the physical write");
        let receipt = retry_authority
            .finalize_namespace_with_physical_frontier(
                retry,
                complete.len() as u64,
                1,
                hash_bytes(complete),
                "explicit recovered settlement".to_owned(),
            )
            .expect("only the recovered exact frontier can settle");
        assert_eq!(receipt.committed_entry_count, Some(1));
        assert!(receipt.physical_frontier_hash.is_some());
        assert_eq!(
            service.quota.lock().expect("quota").workspace_used_bytes(),
            16
        );
        service
            .finalize_namespace(other, "unrelated owner settled".to_owned())
            .expect("unrelated settlement");
    }
}

#[test]
fn session_finalize_quota_failure_releases_holder_but_keeps_uncertain_book_rejected() {
    for physical in [false, true] {
        let root = tempfile::tempdir().expect("isolated authority root");
        let service = AuthorityManagedStorageServiceV1::new_with_state_root(
            table_with_session_grant(),
            authority(),
            root.path(),
        )
        .expect("authority");
        let namespace = CanonicalHash::from_bytes([0x84; 32]);
        let handle = service
            .admit_namespace(
                request(namespace),
                ValidatedStorageAdmissionCapabilityV1::startup_probe(),
            )
            .expect("session holder");
        let handle_id = handle.handle_id.as_str().to_owned();
        service
            .reconcile_namespace_quota(&handle, 64, 1)
            .expect("pending charge");
        fs::create_dir_all(root.path().join("managed").join("session-log"))
            .expect("existing owner directory for no-follow physical resolution");
        let directory = service
            .physical_namespace_directory_for(ManagedStorageSemanticOwnerV1::SessionLog, namespace)
            .expect("physical namespace");
        fs::create_dir_all(&directory).expect("physical namespace fixture");
        let bytes = b"{}\n";
        fs::write(directory.join("records.jsonl"), bytes).expect("actual complete record");
        fs::write(directory.join(".authority-storage.lock"), b"").expect("physical lock");
        service
            .quota
            .lock()
            .expect("quota")
            .inject_next_directory_sync_failure();
        let result = if physical {
            service.finalize_namespace_with_physical_frontier(
                handle,
                bytes.len() as u64,
                1,
                hash_bytes(bytes),
                "quota reconciliation fault".to_owned(),
            )
        } else {
            service.finalize_namespace(handle, "quota release fault".to_owned())
        };
        assert!(matches!(
            result,
            Err(ManagedStorageErrorV1::AuthorityUnavailable)
        ));
        assert!(
            !service
                .table
                .admitted_namespaces
                .lock()
                .expect("admissions")
                .contains_key(&handle_id)
        );
        let owner_key = storage_quota_owner_key(grant().grant_hash, namespace);
        assert!(
            !service
                .active_owners
                .lock()
                .expect("active holders")
                .contains(&owner_key)
        );
        let quota = service.quota.lock().expect("quota");
        assert_eq!(
            quota.workspace_used_bytes(),
            64,
            "failed persistence retains the previous in-memory charge"
        );
        assert!(
            quota.ensure_healthy().is_err(),
            "releasing the holder must not heal uncertain persistence"
        );
        drop(quota);
        let uncertain =
            fs::read(quota_journal_path(root.path())).expect("uncertain installed snapshot");
        assert!(
            matches!(
                service.admit_namespace(
                    request(namespace),
                    ValidatedStorageAdmissionCapabilityV1::startup_probe(),
                ),
                Err(ManagedStorageErrorV1::AuthorityUnavailable)
            ),
            "actual quota uncertainty, not a leaked DuplicateClaim, rejects reuse"
        );
        assert_eq!(
            fs::read(quota_journal_path(root.path())).expect("unchanged uncertainty"),
            uncertain
        );
        assert_eq!(
            fs::read(directory.join("records.jsonl")).expect("recovery bytes"),
            bytes
        );
    }
}

#[test]
fn session_finalize_rejected_handle_does_not_release_the_actual_owner() {
    let service = AuthorityManagedStorageServiceV1::new(table_with_session_grant(), authority());
    let namespace = CanonicalHash::from_bytes([0x85; 32]);
    let handle = service
        .admit_namespace(
            request(namespace),
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        )
        .expect("real holder");
    let mismatched = ManagedStorageNamespaceHandleV1::new(
        OpaqueKernelCapabilityHandleId::new(handle.handle_id.as_str().to_owned()),
        CanonicalHash::from_bytes([0x86; 32]),
        handle.capability_family,
        OpaqueKernelCapabilityAuthenticatorV1::new("mismatched fixture handle".to_owned()),
    );
    assert!(matches!(
        service.finalize_namespace_with_physical_frontier(
            mismatched,
            0,
            0,
            hash_bytes(b""),
            "invalid handle".to_owned(),
        ),
        Err(ManagedStorageErrorV1::CapabilityMismatch)
    ));
    service
        .validate_namespace_write(&handle)
        .expect("invalid request cannot detach the real holder");
    assert!(matches!(
        service.admit_namespace(
            request(namespace),
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        ),
        Err(ManagedStorageErrorV1::DuplicateClaim)
    ));
    service
        .finalize_namespace(handle, "actual holder".to_owned())
        .expect("real owner still settles");
}
