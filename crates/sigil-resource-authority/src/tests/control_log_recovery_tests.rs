use std::{cell::RefCell, sync::mpsc, time::Duration};

use super::*;
use sigil_kernel::managed_storage::StorageAdmissionSourceV1;
use sigil_kernel::resource::{
    ManagedStorageAdmissionPurposeV1, ManagedStorageCapabilityFamilyV1, ResourceAuthorityScopeV1,
    ResourceOwnerScopeV1,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    After(Phase),
    PartialHeader,
    SelectedBeforeRegistry,
    HeaderParentSync,
    HeaderSync,
    HeaderDirectorySync,
    HeaderMarkerSync,
    ActivatedDirectorySync,
}
thread_local! { static FAULT: RefCell<Option<Fault>> = const { RefCell::new(None) }; }
fn take(fault: Fault) -> bool {
    FAULT.with(|slot| {
        if *slot.borrow() == Some(fault) {
            slot.take();
            true
        } else {
            false
        }
    })
}
pub(super) fn fail_selected_publication() -> Result<(), ManagedStorageErrorV1> {
    if take(Fault::SelectedBeforeRegistry) {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    } else {
        Ok(())
    }
}
pub(super) fn fail_header_parent_sync() -> Result<(), ManagedStorageErrorV1> {
    if take(Fault::HeaderParentSync) {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    } else {
        Ok(())
    }
}
pub(super) fn fail_header_marker_sync() -> Result<(), ManagedStorageErrorV1> {
    if take(Fault::HeaderMarkerSync) {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    } else {
        Ok(())
    }
}
pub(super) fn fail_activated_directory_sync() -> Result<(), ManagedStorageErrorV1> {
    if take(Fault::ActivatedDirectorySync) {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    } else {
        Ok(())
    }
}
pub(super) fn fail_after(phase: Phase) -> Result<(), ManagedStorageErrorV1> {
    if take(Fault::After(phase)) {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    } else {
        Ok(())
    }
}
pub(super) fn fail_partial_header(path: &Path, bytes: &[u8]) -> Result<(), ManagedStorageErrorV1> {
    if take(Fault::PartialHeader) {
        write_staging(path, &bytes[..bytes.len() / 2])?;
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    } else {
        Ok(())
    }
}
pub(super) fn fail_header_sync() -> Result<(), ManagedStorageErrorV1> {
    if take(Fault::HeaderSync) {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    } else {
        Ok(())
    }
}
pub(super) fn fail_header_directory_sync() -> Result<(), ManagedStorageErrorV1> {
    if take(Fault::HeaderDirectorySync) {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    } else {
        Ok(())
    }
}

fn source() -> StorageAdmissionSourceV1 {
    super::super::tests::source()
}
fn table() -> AuthorityStorageGrantTableV1 {
    let mut table = AuthorityStorageGrantTableV1::new();
    for (index, owner) in [
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        ManagedStorageSemanticOwnerV1::ApplicationControlRecovery,
    ]
    .into_iter()
    .enumerate()
    {
        let mut grant = super::super::tests::grant();
        grant.semantic_owner = owner;
        grant.grant_id = OpaqueStorageGrantId::new(format!("recovery-grant-{index}"));
        grant.grant_hash = CanonicalHash::from_bytes([index as u8 + 11; 32]);
        grant.quota_profile.max_bytes = 16 * 1024 * 1024;
        grant.quota_profile.max_entries = 1000;
        table.register(grant).expect("grant");
    }
    table
}
fn service(root: &Path) -> AuthorityManagedStorageServiceV1 {
    AuthorityManagedStorageServiceV1::new_with_state_root(
        table(),
        super::super::tests::authority(),
        root,
    )
    .expect("authority")
}
fn admit(
    service: &AuthorityManagedStorageServiceV1,
    owner: ManagedStorageSemanticOwnerV1,
    seed: u8,
) -> ManagedStorageNamespaceHandleV1 {
    let handle = service
        .admit_namespace(
            ManagedStorageAdmissionRequestV1 {
                semantic_owner: owner,
                capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
                purpose: ManagedStorageAdmissionPurposeV1::DurablePayload,
                source: source(),
                owner_scope: ResourceOwnerScopeV1::Application,
                authority_scope: ResourceAuthorityScopeV1::Application,
                namespace_key_hash: CanonicalHash::from_bytes([seed; 32]),
            },
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        )
        .expect("admit");
    let directory = service
        .state_root
        .as_ref()
        .expect("physical root")
        .join("managed")
        .join(storage_owner_leaf(owner).expect("owner leaf"))
        .join(handle.namespace_hash.to_hex());
    fs::create_dir_all(&directory).expect("namespace");
    fs::write(directory.join("authority-admission.json"), serde_json::to_vec(&serde_json::json!({
        "schema_version":3, "handle_id":handle.handle_id.as_str(), "namespace_hash":handle.namespace_hash
    })).expect("marker")).expect("marker write");
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(".authority-storage.lock"))
        .expect("lock file");
    handle
}
fn path(
    service: &AuthorityManagedStorageServiceV1,
    handle: &ManagedStorageNamespaceHandleV1,
) -> PathBuf {
    service
        .physical_namespace_directory(&service.record_for_handle(handle).expect("record"))
        .expect("path")
}

fn preview(
    service: &AuthorityManagedStorageServiceV1,
    recovery: &ManagedStorageNamespaceHandleV1,
    old: &ManagedStorageNamespaceHandleV1,
    next: &ManagedStorageNamespaceHandleV1,
    logical: CanonicalHash,
    from: u64,
) -> (Preview, Vec<u8>) {
    let operation = format!("recover-{from}");
    let (old_byte_length, old_content_digest) =
        physical_digest(&path(service, old)).expect("old physical facts");
    let mut bytes = serde_json::to_vec(&serde_json::json!({
        "schema_version":1, "type":"application_control_header", "logical_journal_id":logical,
        "command_generation":from+1, "operation_id":operation,
        "old_namespace_hash":old.namespace_hash, "old_byte_length":old_byte_length, "old_content_digest":old_content_digest,
    })).expect("header");
    bytes.push(b'\n');
    let preview = service
        .preview_control_log_recovery(
            recovery,
            old,
            next,
            Request {
                logical_journal_id: logical,
                operation_id: operation,
                from_generation: from,
                successor_generation: from + 1,
                header_digest: hash_bytes(&bytes),
                owner_context_digest: hash_bytes(b"test semantic owner context"),
            },
        )
        .expect("preview");
    (preview, bytes)
}

#[test]
fn partial_header_and_sync_failures_resume_same_operation_after_reopen() {
    for fault in [
        Fault::PartialHeader,
        Fault::SelectedBeforeRegistry,
        Fault::HeaderParentSync,
        Fault::HeaderSync,
        Fault::HeaderDirectorySync,
        Fault::HeaderMarkerSync,
        Fault::ActivatedDirectorySync,
        Fault::After(Phase::Prepared),
        Fault::After(Phase::Sealed),
        Fault::After(Phase::HeaderInitialized),
        Fault::After(Phase::Activated),
    ] {
        let temp = tempfile::tempdir().expect("root");
        let authority = service(temp.path());
        let old = admit(
            &authority,
            ManagedStorageSemanticOwnerV1::ApplicationControlLog,
            21,
        );
        let new = admit(
            &authority,
            ManagedStorageSemanticOwnerV1::ApplicationControlLog,
            22,
        );
        let recovery = admit(
            &authority,
            ManagedStorageSemanticOwnerV1::ApplicationControlRecovery,
            23,
        );
        let original = b"{\"done\":true}\n{\"partial\":";
        fs::write(path(&authority, &old).join("records.jsonl"), original).expect("damaged log");
        let (expected, bytes) = preview(&authority, &recovery, &old, &new, old.namespace_hash, 0);
        FAULT.with(|slot| *slot.borrow_mut() = Some(fault));
        assert!(
            authority
                .advance_control_log_recovery(&recovery, &old, &new, &expected, &bytes)
                .is_err()
        );
        assert!(authority.validate_namespace_write(&old).is_err());
        let canonical = path(&authority, &new).join("records.jsonl");
        if matches!(fault, Fault::PartialHeader | Fault::HeaderSync) {
            assert!(
                !canonical.exists(),
                "unpublished header must stay in staging"
            );
        }
        if fault == Fault::ActivatedDirectorySync {
            FAULT.with(|slot| *slot.borrow_mut() = Some(Fault::ActivatedDirectorySync));
            assert!(
                authority.validate_namespace_write(&new).is_err(),
                "visible Activated cannot bypass failed synchronization"
            );
        } else if fault != Fault::After(Phase::Activated) {
            assert!(authority.validate_namespace_write(&new).is_err());
        }
        drop(authority);
        let reopened = service(temp.path());
        let old = admit(
            &reopened,
            ManagedStorageSemanticOwnerV1::ApplicationControlLog,
            21,
        );
        let new = admit(
            &reopened,
            ManagedStorageSemanticOwnerV1::ApplicationControlLog,
            22,
        );
        let recovery = admit(
            &reopened,
            ManagedStorageSemanticOwnerV1::ApplicationControlRecovery,
            23,
        );
        let recovered = reopened
            .query_control_log_recovery(&recovery)
            .expect("query")
            .expect("selected operation");
        assert_eq!(recovered.preview, expected);
        assert_eq!(
            reopened
                .preview_control_log_recovery(&recovery, &old, &new, expected.request.clone())
                .expect("same preview"),
            expected
        );
        let completed = reopened
            .advance_control_log_recovery(&recovery, &old, &new, &expected, &bytes)
            .expect("resume");
        assert_eq!(completed.phase, Phase::Activated);
        assert_eq!(
            fs::read(path(&reopened, &old).join("records.jsonl")).expect("old bytes"),
            original
        );
        assert_eq!(fs::read(&canonical).expect("header"), bytes);
        reopened
            .validate_namespace_write(&new)
            .expect("new generation open");
    }
}

#[test]
fn conflicting_header_or_business_records_are_never_overwritten() {
    let temp = tempfile::tempdir().expect("root");
    let authority = service(temp.path());
    let old = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        21,
    );
    let new = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        22,
    );
    let recovery = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlRecovery,
        23,
    );
    let (expected, bytes) = preview(&authority, &recovery, &old, &new, old.namespace_hash, 0);
    FAULT.with(|slot| *slot.borrow_mut() = Some(Fault::After(Phase::Sealed)));
    assert!(
        authority
            .advance_control_log_recovery(&recovery, &old, &new, &expected, &bytes)
            .is_err()
    );
    let path = path(&authority, &new).join("records.jsonl");
    let occupied = b"{\"business\":true}\n";
    fs::write(&path, occupied).expect("occupied namespace");
    assert!(
        authority
            .advance_control_log_recovery(&recovery, &old, &new, &expected, &bytes)
            .is_err()
    );
    assert_eq!(fs::read(path).expect("preserved"), occupied);
    let mut replacement = expected.request.clone();
    replacement.operation_id = "other-operation".to_owned();
    assert!(
        authority
            .preview_control_log_recovery(&recovery, &old, &new, replacement)
            .is_err()
    );
}

#[test]
fn replacing_original_file_with_identical_bytes_invalidates_preview_identity() {
    let temp = tempfile::tempdir().expect("root");
    let authority = service(temp.path());
    let old = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        21,
    );
    let new = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        22,
    );
    let recovery = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlRecovery,
        23,
    );
    let old_path = path(&authority, &old).join("records.jsonl");
    let original = b"{\"partial\":";
    fs::write(&old_path, original).expect("original bytes");
    let (expected, header) = preview(&authority, &recovery, &old, &new, old.namespace_hash, 0);
    assert!(expected.old_file_identity.is_some());
    // Keep the old inode alive to ensure the replacement is a different physical object.
    let retained = path(&authority, &old).join("retained-original");
    fs::rename(&old_path, &retained).expect("retain original inode");
    fs::write(&old_path, original).expect("identical replacement");
    assert!(
        authority
            .advance_control_log_recovery(&recovery, &old, &new, &expected, &header)
            .is_err()
    );
    assert!(
        authority
            .query_control_log_recovery(&recovery)
            .expect("query")
            .is_none()
    );
    assert_eq!(fs::read(&old_path).expect("preserved"), original);
    assert!(!path(&authority, &new).join("records.jsonl").exists());
}

#[test]
fn activated_retry_preserves_later_business_bytes_and_multigeneration_chain_is_verified() {
    let temp = tempfile::tempdir().expect("root");
    let authority = service(temp.path());
    let old = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        21,
    );
    let new = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        22,
    );
    let third = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        24,
    );
    let recovery = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlRecovery,
        23,
    );
    let logical = old.namespace_hash;
    let (first, bytes) = preview(&authority, &recovery, &old, &new, logical, 0);
    let activated = authority
        .advance_control_log_recovery(&recovery, &old, &new, &first, &bytes)
        .expect("activated");
    let mut business = bytes.clone();
    business.extend_from_slice(b"{\"business\":1}\n{\"bad-tail\":");
    fs::write(path(&authority, &new).join("records.jsonl"), &business).expect("later records");
    assert_eq!(
        authority
            .advance_control_log_recovery(&recovery, &old, &new, &first, b"must not initialize")
            .expect("idempotent"),
        activated
    );
    assert_eq!(
        fs::read(path(&authority, &new).join("records.jsonl")).expect("retained"),
        business
    );
    let (second, header) = preview(&authority, &recovery, &new, &third, logical, 1);
    authority
        .advance_control_log_recovery(&recovery, &new, &third, &second, &header)
        .expect("second rotation");
    assert_eq!(
        authority
            .query_control_log_recovery(&recovery)
            .expect("chain")
            .expect("current")
            .preview,
        second
    );
    fs::remove_file(path(&authority, &recovery).join(phase_name(0, Phase::HeaderInitialized)))
        .expect("missing phase");
    assert!(authority.query_control_log_recovery(&recovery).is_err());
    assert!(authority.validate_namespace_write(&third).is_err());
}

#[cfg(windows)]
#[test]
fn windows_recovery_activates_successor_without_directory_flush_support() {
    let temp = tempfile::tempdir().expect("root");
    let authority = service(temp.path());
    let old = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        61,
    );
    let successor = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        62,
    );
    let recovery = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlRecovery,
        63,
    );
    let old_path = path(&authority, &old).join("records.jsonl");
    let old_bytes = b"{\"command\":1}\n{partial";
    fs::write(&old_path, old_bytes).expect("old journal");
    let (review, header) = preview(
        &authority,
        &recovery,
        &old,
        &successor,
        old.namespace_hash,
        0,
    );

    let state = authority
        .advance_control_log_recovery(&recovery, &old, &successor, &review, &header)
        .expect("activate exact successor on Windows");
    assert_eq!(state.phase, Phase::Activated);
    assert_eq!(fs::read(old_path).expect("old journal"), old_bytes);
    assert_eq!(
        fs::read(path(&authority, &successor).join("records.jsonl")).expect("new journal"),
        header
    );
    assert_eq!(
        authority
            .query_control_log_recovery(&recovery)
            .expect("durable chain")
            .expect("activated phase"),
        state
    );
}

#[test]
fn forward_guard_blocks_sealing_until_the_authorized_dispatch_releases_ownership() {
    let temp = tempfile::tempdir().expect("root");
    let authority = Arc::new(service(temp.path()));
    let old = Arc::new(admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        21,
    ));
    let new = Arc::new(admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        22,
    ));
    let recovery = Arc::new(admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlRecovery,
        23,
    ));
    let (expected, bytes) = preview(&authority, &recovery, &old, &new, old.namespace_hash, 0);
    let guard = authority.acquire_forward_guard(&old).expect("guard");
    let (sent, received) = mpsc::channel();
    let cloned = (
        authority.clone(),
        old.clone(),
        new.clone(),
        recovery.clone(),
    );
    let join = std::thread::spawn(move || {
        let (authority, old, new, recovery) = cloned;
        sent.send(authority.advance_control_log_recovery(&recovery, &old, &new, &expected, &bytes))
            .expect("result");
    });
    assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
    drop(guard);
    assert_eq!(
        received
            .recv_timeout(Duration::from_secs(5))
            .expect("rotation completes")
            .expect("success")
            .phase,
        Phase::Activated
    );
    join.join().expect("thread");
    assert!(authority.acquire_forward_guard(&old).is_err());
}

#[test]
fn forward_guard_retains_namespace_and_process_ownership_after_detach_and_service_drop() {
    let temp = tempfile::tempdir().expect("root");
    let authority = service(temp.path());
    let old = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        21,
    );
    authority
        .reserve_namespace_quota_capacity(&old, 100, 100, 2)
        .expect("charge");
    let record = authority.record_for_handle(&old).expect("record");
    let request = record.request.clone();
    let key = storage_quota_owner_key(record.grant.grant_hash, old.namespace_hash);
    drop(record);
    let before = authority
        .quota
        .lock()
        .expect("quota")
        .reservation_for_owner(&key)
        .expect("reserved");
    let guard = authority
        .acquire_forward_guard(&old)
        .expect("dispatch guard");
    let peer = service(temp.path());
    authority.detach_namespace(old).expect("detach");
    assert_eq!(
        peer.admit_namespace(
            request.clone(),
            ValidatedStorageAdmissionCapabilityV1::startup_probe()
        )
        .expect_err("dispatch guard retains the live owner"),
        ManagedStorageErrorV1::DuplicateClaim
    );
    drop(authority);
    drop(peer);
    let reopened = service(temp.path());
    assert_eq!(
        reopened
            .admit_namespace(
                request,
                ValidatedStorageAdmissionCapabilityV1::startup_probe()
            )
            .expect_err("dispatch guard also keeps the shared process ownership alive"),
        ManagedStorageErrorV1::DuplicateClaim
    );
    assert_eq!(
        reopened
            .quota
            .lock()
            .expect("quota")
            .reservation_for_owner(&key),
        Some(before),
        "reopening while a dispatch is active must not cold-reset its charge"
    );
    drop(guard);
    let next = admit(
        &reopened,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        21,
    );
    let next_guard = reopened
        .acquire_forward_guard(&next)
        .expect("physical lock released");
    assert_eq!(
        reopened
            .quota
            .lock()
            .expect("quota")
            .reservation_for_owner(&key),
        Some(before)
    );
    drop(next_guard);
}

#[test]
fn detach_and_reattach_preserve_existing_capacity_and_duplicate_admission_does_not_release_it() {
    let temp = tempfile::tempdir().expect("root");
    let authority = service(temp.path());
    let peer = service(temp.path());
    let old = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        21,
    );
    authority
        .reserve_namespace_quota_capacity(&old, 100, 100, 2)
        .expect("charge");
    let record = authority.record_for_handle(&old).expect("record");
    let key = storage_quota_owner_key(record.grant.grant_hash, old.namespace_hash);
    let before = authority
        .quota
        .lock()
        .expect("quota")
        .reservation_for_owner(&key)
        .expect("reserved");
    for service in [&authority, &peer] {
        assert_eq!(
            service
                .admit_namespace(
                    record.request.clone(),
                    ValidatedStorageAdmissionCapabilityV1::startup_probe()
                )
                .expect_err("a live holder cannot be duplicated across authority services"),
            ManagedStorageErrorV1::DuplicateClaim
        );
    }
    assert_eq!(
        authority
            .quota
            .lock()
            .expect("quota")
            .reservation_for_owner(&key),
        Some(before)
    );
    authority.detach_namespace(old).expect("detach");
    assert_eq!(
        peer.admit_namespace(
            record.request.clone(),
            ValidatedStorageAdmissionCapabilityV1::startup_probe()
        )
        .expect_err("an in-flight authority operation still owns its record"),
        ManagedStorageErrorV1::DuplicateClaim
    );
    drop(record);
    let reattached = admit(
        &peer,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        21,
    );
    peer.validate_namespace_write(&reattached)
        .expect("reattached");
    assert_eq!(
        authority
            .quota
            .lock()
            .expect("quota")
            .reservation_for_owner(&key),
        Some(before)
    );
    drop(peer);
    let after_service_drop = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        21,
    );
    authority
        .validate_namespace_write(&after_service_drop)
        .expect("dropping a service releases its live holder without discarding capacity");
    assert_eq!(
        authority
            .quota
            .lock()
            .expect("quota")
            .reservation_for_owner(&key),
        Some(before)
    );
}

#[test]
fn registry_reopen_recharges_real_metadata_and_finalization_reports_its_frontier() {
    let temp = tempfile::tempdir().expect("root");
    let authority = service(temp.path());
    let old = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        21,
    );
    let new = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        22,
    );
    let recovery = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlRecovery,
        23,
    );
    let (expected, bytes) = preview(&authority, &recovery, &old, &new, old.namespace_hash, 0);
    authority
        .advance_control_log_recovery(&recovery, &old, &new, &expected, &bytes)
        .expect("rotate");
    let registry = path(&authority, &recovery);
    let actual_bytes: u64 = PHASES
        .into_iter()
        .map(|phase| {
            fs::metadata(registry.join(phase_name(0, phase)))
                .expect("phase metadata")
                .len()
        })
        .sum();
    assert!(actual_bytes > 0);
    drop(authority);
    let reopened = service(temp.path());
    let recovery = admit(
        &reopened,
        ManagedStorageSemanticOwnerV1::ApplicationControlRecovery,
        23,
    );
    reopened
        .query_control_log_recovery(&recovery)
        .expect("recharged query");
    let record = reopened.record_for_handle(&recovery).expect("record");
    let charge = reopened
        .quota
        .lock()
        .expect("quota")
        .reservation_for_owner(&storage_quota_owner_key(
            record.grant.grant_hash,
            record.namespace_hash,
        ))
        .expect("physical metadata reservation");
    assert_eq!(charge.reserved_bytes, actual_bytes);
    assert_eq!(charge.reserved_entries, 4);
    let receipt = reopened
        .finalize_namespace(recovery, "registry fixture complete".to_owned())
        .expect("settlement");
    assert_eq!(receipt.committed_entry_count, Some(4));
    assert!(receipt.physical_frontier_hash.is_some());
}

#[test]
fn changed_old_frontier_is_rejected_before_sealing() {
    let temp = tempfile::tempdir().expect("root");
    let authority = service(temp.path());
    let old = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        21,
    );
    let new = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        22,
    );
    let recovery = admit(
        &authority,
        ManagedStorageSemanticOwnerV1::ApplicationControlRecovery,
        23,
    );
    let (expected, bytes) = preview(&authority, &recovery, &old, &new, old.namespace_hash, 0);
    fs::write(
        path(&authority, &old).join("records.jsonl"),
        b"{\"new\":true}\n",
    )
    .expect("concurrent old write");
    assert!(
        authority
            .advance_control_log_recovery(&recovery, &old, &new, &expected, &bytes)
            .is_err()
    );
    authority
        .validate_namespace_write(&old)
        .expect("preview did not seal old");
    assert!(authority.validate_namespace_write(&new).is_err());
    assert!(
        authority
            .query_control_log_recovery(&recovery)
            .expect("query")
            .is_none()
    );
}
