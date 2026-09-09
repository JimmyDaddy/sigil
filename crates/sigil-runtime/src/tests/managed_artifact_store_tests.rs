use std::sync::Arc;

use sigil_kernel::managed_storage::{
    ManagedStorageAdmissionRequestV1, ManagedStorageErrorV1, ManagedStorageNamespaceHandleV1,
    ManagedStorageServiceV1, ManagedStorageStorageReceiptV1, ValidatedStorageAdmissionCapabilityV1,
};
use sigil_kernel::resource::{AuthorityGeneration, CanonicalHash};
use sigil_resource_authority::storage::{
    AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
};

use super::*;

fn writer(root: &Path) -> Arc<ManagedStorageWriterAdapterV1> {
    configured_writer(
        root,
        TOOL_ARTIFACT_SESSION_BUDGET_BYTES,
        TOOL_ARTIFACT_SESSION_BUDGET_BYTES,
        false,
    )
}

fn configured_writer(
    root: &Path,
    staging_limit: u64,
    store_limit: u64,
    durable: bool,
) -> Arc<ManagedStorageWriterAdapterV1> {
    configured_writer_with_validation_gate(root, staging_limit, store_limit, durable, None)
}

fn configured_writer_with_validation_gate(
    root: &Path,
    staging_limit: u64,
    store_limit: u64,
    durable: bool,
    write_allowed: Option<Arc<std::sync::atomic::AtomicBool>>,
) -> Arc<ManagedStorageWriterAdapterV1> {
    let generation = AuthorityGeneration {
        epoch: 1,
        instance_hash: CanonicalHash::from_bytes([0x28; 32]),
    };
    let cutover_manifest_hash = CanonicalHash::from_bytes([0x84; 32]);
    let mut table = AuthorityStorageGrantTableV1::new();
    let mut staging_grant = crate::managed_storage_writer::grant_for_channel_with_context(
        StorageWriterChannelV1::ArtifactStaging,
        0x81,
        generation,
        cutover_manifest_hash,
    );
    let mut store_grant = crate::managed_storage_writer::grant_for_channel_with_context(
        StorageWriterChannelV1::ArtifactStore,
        0x82,
        generation,
        cutover_manifest_hash,
    );
    staging_grant.quota_profile.max_bytes = staging_limit;
    store_grant.quota_profile.max_bytes = store_limit;
    table
        .register(staging_grant.clone())
        .expect("staging grant");
    table.register(store_grant.clone()).expect("store grant");
    let service: Arc<dyn ManagedStorageServiceV1> = if durable {
        Arc::new(
            AuthorityManagedStorageServiceV1::new_with_state_root(table, generation, root)
                .expect("durable authority"),
        )
    } else {
        Arc::new(AuthorityManagedStorageServiceV1::new(table, generation))
    };
    let service: Arc<dyn ManagedStorageServiceV1> = if let Some(write_allowed) = write_allowed {
        Arc::new(ValidationGateStorageService {
            inner: service,
            write_allowed,
        })
    } else {
        service
    };
    Arc::new(
        ManagedStorageWriterAdapterV1::new(
            service,
            root.to_path_buf(),
            CanonicalHash::from_bytes([0x84; 32]),
        )
        .with_artifact_retire_authority(Arc::new(
            sigil_resource_authority::maintenance::ArtifactRetireAuthorityV1::new(
                generation,
                staging_grant.grant_hash,
                store_grant.grant_hash,
            ),
        )),
    )
}

/// Fault seam for the cheap authority-health rejection; real quota operations remain delegated.
struct ValidationGateStorageService {
    inner: Arc<dyn ManagedStorageServiceV1>,
    write_allowed: Arc<std::sync::atomic::AtomicBool>,
}

impl ManagedStorageServiceV1 for ValidationGateStorageService {
    fn admit_namespace(
        &self,
        request: ManagedStorageAdmissionRequestV1,
        capability: ValidatedStorageAdmissionCapabilityV1,
    ) -> Result<ManagedStorageNamespaceHandleV1, ManagedStorageErrorV1> {
        self.inner.admit_namespace(request, capability)
    }

    fn validate_namespace_write(
        &self,
        handle: &ManagedStorageNamespaceHandleV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        if !self
            .write_allowed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(ManagedStorageErrorV1::AuthorityUnavailable);
        }
        self.inner.validate_namespace_write(handle)
    }

    fn reconcile_namespace_quota(
        &self,
        handle: &ManagedStorageNamespaceHandleV1,
        bytes: u64,
        entries: u64,
    ) -> Result<(), ManagedStorageErrorV1> {
        self.inner.reconcile_namespace_quota(handle, bytes, entries)
    }

    fn reserve_namespace_quota_capacity(
        &self,
        handle: &ManagedStorageNamespaceHandleV1,
        minimum_bytes: u64,
        preferred_bytes: u64,
        entries: u64,
    ) -> Result<u64, ManagedStorageErrorV1> {
        self.inner
            .reserve_namespace_quota_capacity(handle, minimum_bytes, preferred_bytes, entries)
    }

    fn finalize_namespace(
        &self,
        handle: ManagedStorageNamespaceHandleV1,
        reason: String,
    ) -> Result<ManagedStorageStorageReceiptV1, ManagedStorageErrorV1> {
        self.inner.finalize_namespace(handle, reason)
    }

    fn finalize_namespace_with_physical_frontier(
        &self,
        handle: ManagedStorageNamespaceHandleV1,
        byte_length: u64,
        record_count: u64,
        content_hash: CanonicalHash,
        reason: String,
    ) -> Result<ManagedStorageStorageReceiptV1, ManagedStorageErrorV1> {
        self.inner.finalize_namespace_with_physical_frontier(
            handle,
            byte_length,
            record_count,
            content_hash,
            reason,
        )
    }
}

#[test]
fn managed_artifact_capture_publish_read_and_stale_write_fail_closed() {
    let root = tempfile::tempdir().expect("tempdir");
    let writer = writer(root.path());
    let lease =
        ManagedArtifactStoreLeaseV1::acquire(writer, "session-artifact", "session-artifact-id")
            .expect("artifact lease");
    let store = lease.store();
    let descriptor = store
        .capture_text(
            "call-1",
            "shell",
            "managed artifact",
            sigil_kernel::ToolArtifactSensitivity::Ordinary,
        )
        .expect("capture");
    assert_eq!(
        store.read_all(&descriptor).expect("read"),
        b"managed artifact"
    );
    assert_eq!(
        store.resolve(&descriptor.artifact_ref).expect("resolve"),
        descriptor
    );
    assert_eq!(store.manifest_inventory().expect("inventory").len(), 1);
    let page = store
        .read_page(
            &descriptor.artifact_ref,
            sigil_kernel::ToolArtifactSelectorV1::ByteSlice {
                offset: 0,
                limit: 7,
            },
        )
        .expect("page");
    assert_eq!(page.body, "managed");
    lease.finalize().expect("finalize");
    let error = store
        .capture_text(
            "call-2",
            "shell",
            "stale mutation",
            sigil_kernel::ToolArtifactSensitivity::Ordinary,
        )
        .expect_err("settled artifact handle must reject writes");
    assert!(error.to_string().contains("closed") || error.to_string().contains("rejected"));
}

#[test]
fn managed_process_capture_uses_opaque_staging_backend() {
    let root = tempfile::tempdir().expect("tempdir");
    let writer = writer(root.path());
    let lease =
        ManagedArtifactStoreLeaseV1::acquire(writer, "session-process", "session-process-id")
            .expect("artifact lease");
    let sink = lease
        .store()
        .begin_policy_safe_capture(
            "call-1",
            "shell",
            "text/plain",
            sigil_kernel::ToolArtifactEncoding::Utf8,
            sigil_kernel::ToolArtifactSensitivity::Ordinary,
        )
        .begin_process_capture(ProcessStreamCaptureConfigV1 {
            stream_layout: sigil_kernel::ToolOutputStreamLayoutV1::SeparatePipesNoCrossStreamOrder,
            preview_limit_bytes_per_stream: 1024,
            artifact_payload_limit_bytes_combined: 1024,
            artifact_reservation_stdout_bytes: 512,
            artifact_reservation_stderr_bytes: 512,
            artifact_staging_limit_bytes_per_stream: 512,
            observed_limit_bytes_combined: 2048,
        })
        .expect("staging");
    let mut sink = sink;
    sink.write_stream(sigil_kernel::ToolOutputStreamV1::Stdout, b"out")
        .expect("stdout");
    sink.write_stream(sigil_kernel::ToolOutputStreamV1::Stderr, b"err")
        .expect("stderr");
    let (descriptor, segments, _) = sink
        .finish_process_capture(6, 0, sigil_kernel::ToolSourceCompletenessV1::Complete)
        .expect("finish");
    assert_eq!(descriptor.persisted_bytes, 6);
    assert_eq!(segments[0].persisted_bytes, 3);
    assert_eq!(segments[1].persisted_bytes, 3);
}

#[test]
fn managed_artifact_gc_and_trash_prune_consume_authority_frontier() {
    let root = tempfile::tempdir().expect("tempdir");
    let writer = writer(root.path());
    let lease =
        ManagedArtifactStoreLeaseV1::acquire(Arc::clone(&writer), "session-gc", "session-gc-id")
            .expect("artifact lease");
    let store = lease.store();
    let descriptor = store
        .capture_text(
            "call-gc",
            "shell",
            "retire me",
            sigil_kernel::ToolArtifactSensitivity::Ordinary,
        )
        .expect("capture");
    let refs = vec![descriptor.artifact_ref.clone()];
    let report = store
        .garbage_collect_with_retire_frontier(
            &ToolArtifactGcRootsV1::default(),
            u64::MAX,
            sigil_kernel::session::TOOL_ARTIFACT_ORPHAN_GRACE_MS,
            ToolArtifactRetireFrontierV1 {
                selected_refs_hash: artifact_refs_hash(&refs),
                selected_count: 1,
                selected_bytes: descriptor.persisted_bytes,
                eligibility_frontier: 1,
                policy_hash: canonical_sha256(b"gc-policy"),
            },
        )
        .expect("managed GC");
    assert_eq!(report.tombstoned_refs, refs);
    assert!(store.resolve(&descriptor.artifact_ref).is_err());
    let pruned = store
        .prune_garbage_trash(
            u64::MAX,
            sigil_kernel::session::TOOL_ARTIFACT_ORPHAN_GRACE_MS,
        )
        .expect("managed trash prune");
    assert_eq!(pruned.removed_tombstones, 1);
}

fn capture_config(per_stream_limit: u64) -> ProcessStreamCaptureConfigV1 {
    ProcessStreamCaptureConfigV1 {
        stream_layout: sigil_kernel::ToolOutputStreamLayoutV1::SeparatePipesNoCrossStreamOrder,
        preview_limit_bytes_per_stream: 1024,
        artifact_payload_limit_bytes_combined: per_stream_limit.saturating_mul(2),
        artifact_reservation_stdout_bytes: per_stream_limit,
        artifact_reservation_stderr_bytes: per_stream_limit,
        artifact_staging_limit_bytes_per_stream: per_stream_limit,
        observed_limit_bytes_combined: u64::MAX,
    }
}

fn durable_quota_usage(root: &Path) -> u64 {
    sigil_resource_authority::quota::QuotaBookV1::open_existing(
        root.join(".authority-quota").join("managed-storage.json"),
    )
    .expect("replay current quota")
    .workspace_used_bytes()
}

fn durable_quota_record_count(root: &Path) -> usize {
    let snapshot: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join(".authority-quota").join("managed-storage.json"))
            .expect("quota snapshot"),
    )
    .expect("quota JSON");
    snapshot["records"].as_array().expect("records").len()
}

#[test]
fn managed_capture_many_chunks_amortize_inventory_and_durable_reservations() {
    let root = tempfile::tempdir().expect("tempdir");
    let writer = configured_writer(root.path(), 8 * 1024 * 1024, 8 * 1024 * 1024, true);
    let lease =
        ManagedArtifactStoreLeaseV1::acquire(writer, "chunked", "chunked-session").expect("lease");
    let limit = 2 * 1024 * 1024;
    let mut capture = lease
        .backend
        .clone()
        .begin_process_capture(capture_config(limit))
        .expect("capture");
    let before_records = durable_quota_record_count(root.path());
    let initial_scans = lease
        .backend
        .inventory_scans
        .load(std::sync::atomic::Ordering::Relaxed);
    let bytes = [b'x'; 256];
    for _ in 0..8192 {
        capture
            .write_stream(ToolOutputStreamV1::Stdout, &bytes, limit)
            .expect("stdout chunk");
        capture
            .write_stream(ToolOutputStreamV1::Stderr, &bytes, limit)
            .expect("stderr chunk");
    }
    assert_eq!(
        lease
            .backend
            .inventory_scans
            .load(std::sync::atomic::Ordering::Relaxed),
        initial_scans,
        "chunks must not inventory the namespace"
    );
    assert!(
        durable_quota_record_count(root.path()) - before_records <= 6,
        "16,384 chunks need only bounded capacity growth and stream entry admission"
    );
    assert_eq!(durable_quota_usage(root.path()), 4 * 1024 * 1024);
    let snapshot = capture.finish().expect("finish capture");
    assert_eq!(snapshot.stdout_bytes, vec![b'x'; limit as usize]);
    assert_eq!(snapshot.stderr_bytes, vec![b'x'; limit as usize]);
    assert_eq!(snapshot.stdout_observed_bytes, limit);
    assert_eq!(snapshot.stderr_observed_bytes, limit);
    assert_eq!(durable_quota_usage(root.path()), 0);
    let (staging, _) = lease.backend.live_roots().expect("live roots");
    assert_eq!(
        directory_file_usage(&staging.join("staging")).expect("physical usage"),
        (0, 0)
    );
}

#[test]
fn managed_capture_spare_capacity_is_capped_by_capture_limit_and_drop_reclaims_it() {
    let root = tempfile::tempdir().expect("tempdir");
    let writer = configured_writer(root.path(), 1024 * 1024, 1024 * 1024, true);
    let lease =
        ManagedArtifactStoreLeaseV1::acquire(writer, "small", "small-session").expect("lease");
    let mut capture = lease
        .backend
        .clone()
        .begin_process_capture(capture_config(32))
        .expect("capture");
    capture
        .write_stream(ToolOutputStreamV1::Stdout, b"a", 32)
        .expect("small write");
    assert_eq!(
        durable_quota_usage(root.path()),
        64,
        "small calls must not reserve a full batch"
    );
    assert_eq!(
        capture
            .write_stream(ToolOutputStreamV1::Stdout, &[b'b'; 64], u64::MAX)
            .expect("bounded write"),
        (65, true)
    );
    let (staging, _) = lease.backend.live_roots().expect("live roots");
    assert_eq!(
        directory_file_usage(&staging.join("staging"))
            .expect("bounded staging")
            .0,
        32
    );
    drop(capture);
    assert_eq!(durable_quota_usage(root.path()), 0);
    assert_eq!(
        directory_file_usage(&staging.join("staging")).expect("drop cleanup"),
        (0, 0)
    );
}

#[test]
fn managed_capture_quota_failure_preserves_prefix_and_cannot_finish_complete() {
    let root = tempfile::tempdir().expect("tempdir");
    let writer = configured_writer(root.path(), 5, 100, true);
    let lease =
        ManagedArtifactStoreLeaseV1::acquire(writer, "limited", "limited-session").expect("lease");
    let mut capture = lease
        .backend
        .clone()
        .begin_process_capture(capture_config(100))
        .expect("capture");
    capture
        .write_stream(ToolOutputStreamV1::Stdout, b"1234", 100)
        .expect("admitted prefix");
    assert_eq!(durable_quota_usage(root.path()), 5);
    let error = capture
        .write_stream(ToolOutputStreamV1::Stdout, b"56", 100)
        .expect_err("must reserve before write");
    assert!(matches!(
        error.downcast_ref::<crate::managed_storage_writer::ManagedStorageWriterErrorV1>(),
        Some(
            crate::managed_storage_writer::ManagedStorageWriterErrorV1::QuotaExceeded {
                dimension: sigil_kernel::managed_storage::ManagedStorageQuotaDimensionV1::Bytes,
                requested: 6,
                limit: 5,
            }
        )
    ));
    let (staging, _) = lease.backend.live_roots().expect("live roots");
    assert_eq!(
        directory_file_usage(&staging.join("staging")).expect("unchanged prefix"),
        (4, 1)
    );
    let records = durable_quota_record_count(root.path());
    for _ in 0..1000 {
        assert!(
            capture
                .write_stream(ToolOutputStreamV1::Stdout, b"discarded", 100)
                .is_err()
        );
    }
    assert_eq!(
        durable_quota_record_count(root.path()),
        records,
        "draining after failure cannot retry journal writes"
    );
    assert_eq!(
        durable_quota_usage(root.path()),
        5,
        "failed growth retains the old capacity until cleanup"
    );
    assert!(
        capture.finish().is_err(),
        "an incomplete storage prefix is not a complete capture"
    );
    assert_eq!(durable_quota_usage(root.path()), 0);
    assert_eq!(
        directory_file_usage(&staging.join("staging")).expect("failure cleanup"),
        (0, 0)
    );
}

#[test]
fn managed_concurrent_captures_share_capacity_and_settle_independently() {
    let root = tempfile::tempdir().expect("tempdir");
    let limit = 16 * 1024;
    let writer = configured_writer(root.path(), 4 * limit, 4 * limit, true);
    let lease = ManagedArtifactStoreLeaseV1::acquire(writer, "parallel", "parallel-session")
        .expect("lease");
    let start = Arc::new(std::sync::Barrier::new(4));
    let mut threads = Vec::new();
    for byte in b'a'..=b'd' {
        let mut capture = lease
            .backend
            .clone()
            .begin_process_capture(capture_config(limit))
            .expect("capture");
        let start = Arc::clone(&start);
        threads.push(std::thread::spawn(move || {
            start.wait();
            for _ in 0..1024 {
                capture
                    .write_stream(ToolOutputStreamV1::Stdout, &[byte; 16], limit)
                    .expect("parallel chunk");
            }
            (byte, capture)
        }));
    }
    let mut captures: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().expect("writer thread"))
        .collect();
    assert_eq!(durable_quota_usage(root.path()), 4 * limit);
    assert_eq!(
        lease
            .backend
            .inventory_scans
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    let (staging, _) = lease.backend.live_roots().expect("live roots");
    assert_eq!(
        directory_file_usage(&staging.join("staging")).expect("concurrent usage"),
        (4 * limit, 4)
    );
    drop(captures.pop());
    assert_eq!(durable_quota_usage(root.path()), 3 * limit);
    for (byte, capture) in captures {
        assert_eq!(
            capture
                .finish()
                .expect("finish independent capture")
                .stdout_bytes,
            vec![byte; limit as usize]
        );
    }
    assert_eq!(durable_quota_usage(root.path()), 0);
}

#[test]
fn managed_capture_capacity_is_shared_across_namespace_owners_without_overcommit() {
    let root = tempfile::tempdir().expect("tempdir");
    let writer = configured_writer(root.path(), 8, 100, true);
    let first = ManagedArtifactStoreLeaseV1::acquire(Arc::clone(&writer), "first", "first-session")
        .expect("first lease");
    let second = ManagedArtifactStoreLeaseV1::acquire(writer, "second", "second-session")
        .expect("second lease");
    let mut first_capture = first
        .backend
        .clone()
        .begin_process_capture(capture_config(100))
        .expect("first capture");
    first_capture
        .write_stream(ToolOutputStreamV1::Stdout, b"1234", 100)
        .expect("first prefix");
    let mut second_capture = second
        .backend
        .clone()
        .begin_process_capture(capture_config(100))
        .expect("second capture");
    assert!(
        second_capture
            .write_stream(ToolOutputStreamV1::Stdout, b"x", 100)
            .is_err()
    );
    assert_eq!(durable_quota_usage(root.path()), 8);
    let (second_staging, _) = second.backend.live_roots().expect("second roots");
    assert_eq!(
        directory_file_usage(&second_staging.join("staging")).expect("unadmitted body"),
        (0, 0)
    );
    drop(second_capture);
    drop(first_capture);
    assert_eq!(durable_quota_usage(root.path()), 0);
    let mut retried = second
        .backend
        .clone()
        .begin_process_capture(capture_config(100))
        .expect("new capture after release");
    retried
        .write_stream(ToolOutputStreamV1::Stdout, b"12345678", 100)
        .expect("released capacity is reusable");
    assert_eq!(retried.finish().expect("finish").stdout_bytes, b"12345678");
}

#[test]
fn managed_capture_cold_admission_inventories_leftover_prefix_before_new_write() {
    let root = tempfile::tempdir().expect("tempdir");
    let writer = configured_writer(root.path(), 6, 100, true);
    let lease =
        ManagedArtifactStoreLeaseV1::acquire(Arc::clone(&writer), "reopen", "reopen-session")
            .expect("lease");
    let backend = Arc::clone(&lease.backend);
    let mut stale = backend
        .clone()
        .begin_process_capture(capture_config(100))
        .expect("capture");
    stale
        .write_stream(ToolOutputStreamV1::Stdout, b"1234", 100)
        .expect("persist prefix");
    let (staging, _) = backend.live_roots().expect("roots");
    lease.finalize().expect("retire the old namespace handle");
    assert!(
        stale
            .write_stream(ToolOutputStreamV1::Stdout, b"5", 100)
            .is_err(),
        "capacity cannot bypass stale handle validation"
    );
    drop(stale);
    drop(backend);
    drop(writer);
    assert_eq!(
        directory_file_usage(&staging.join("staging")).expect("leftover prefix"),
        (4, 1)
    );

    let writer = configured_writer(root.path(), 6, 100, true);
    let reopened = ManagedArtifactStoreLeaseV1::acquire(writer, "reopen", "reopen-session")
        .expect("reopened lease");
    let mut capture = reopened
        .backend
        .clone()
        .begin_process_capture(capture_config(100))
        .expect("reopened capture");
    assert_eq!(
        durable_quota_usage(root.path()),
        4,
        "physical prefix is admitted before new bytes"
    );
    let error = capture
        .write_stream(ToolOutputStreamV1::Stdout, b"789", 100)
        .expect_err("old prefix consumes capacity");
    assert!(matches!(
        error.downcast_ref::<crate::managed_storage_writer::ManagedStorageWriterErrorV1>(),
        Some(crate::managed_storage_writer::ManagedStorageWriterErrorV1::QuotaExceeded { .. })
    ));
    assert_eq!(
        directory_file_usage(&staging.join("staging")).expect("unchanged prefix"),
        (4, 1)
    );
    drop(capture);
    assert_eq!(durable_quota_usage(root.path()), 4);
}

#[test]
fn managed_same_namespace_concurrent_backend_admission_has_one_owner() {
    let root = tempfile::tempdir().expect("tempdir");
    let writers = [
        configured_writer(root.path(), 64, 100, true),
        configured_writer(root.path(), 64, 100, true),
    ];
    let start = Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = writers
        .into_iter()
        .map(|writer| {
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                ManagedArtifactStoreLeaseV1::acquire(writer, "same-key", "same-session")
            })
        })
        .collect();
    let mut admitted = Vec::new();
    let mut rejected = 0;
    for thread in threads {
        match thread.join().expect("admission thread") {
            Ok(lease) => admitted.push(lease),
            Err(_) => rejected += 1,
        }
    }
    assert_eq!(
        rejected, 1,
        "independent services cannot duplicate the namespace owner"
    );
    assert_eq!(admitted.len(), 1);
    let lease = admitted.pop().expect("sole namespace owner");
    let mut capture = lease
        .backend
        .clone()
        .begin_process_capture(capture_config(100))
        .expect("surviving capture");
    capture
        .write_stream(ToolOutputStreamV1::Stdout, b"still charged", 100)
        .expect("surviving owner can write");
    assert_eq!(durable_quota_usage(root.path()), 64);
    assert_eq!(
        capture
            .finish()
            .expect("surviving owner finishes")
            .stdout_bytes,
        b"still charged"
    );
    assert_eq!(durable_quota_usage(root.path()), 0);
}

#[test]
fn managed_capture_file_creation_failure_keeps_quota_until_cleanup() {
    let root = tempfile::tempdir().expect("tempdir");
    let writer = configured_writer(root.path(), 64, 100, true);
    let lease =
        ManagedArtifactStoreLeaseV1::acquire(writer, "io-failure", "io-session").expect("lease");
    let mut capture = lease
        .backend
        .clone()
        .begin_process_capture(capture_config(32))
        .expect("capture");
    let (staging, _) = lease.backend.live_roots().expect("roots");
    let directory = staging.join("staging");
    fs::remove_dir(&directory).expect("remove empty staging fixture");
    fs::write(&directory, b"").expect("block file creation with a regular file");
    assert!(
        capture
            .write_stream(ToolOutputStreamV1::Stdout, b"prefix", 32)
            .is_err()
    );
    assert_eq!(
        durable_quota_usage(root.path()),
        64,
        "admission precedes physical file creation"
    );
    fs::remove_file(&directory).expect("remove test blocker");
    create_private_dir(&directory).expect("restore staging directory");
    assert!(
        capture
            .write_stream(ToolOutputStreamV1::Stdout, b"later", 32)
            .is_err(),
        "storage failure stays terminal for this capture"
    );
    assert!(capture.finish().is_err());
    assert_eq!(durable_quota_usage(root.path()), 0);
    assert_eq!(directory_file_usage(&directory).expect("cleanup"), (0, 0));
}

#[test]
fn managed_artifact_publish_quota_denial_returns_unused_staging_admission() {
    let root = tempfile::tempdir().expect("tempdir");
    let writer = configured_writer(root.path(), 64, 4, true);
    let lease = ManagedArtifactStoreLeaseV1::acquire(writer, "publish-denied", "publish-session")
        .expect("lease");
    let bytes = b"12345";
    let error = lease
        .backend
        .publish_blob(&hash_bytes(bytes), bytes)
        .expect_err("store quota denial");
    assert!(matches!(
        error.downcast_ref::<crate::managed_storage_writer::ManagedStorageWriterErrorV1>(),
        Some(crate::managed_storage_writer::ManagedStorageWriterErrorV1::QuotaExceeded { .. })
    ));
    assert_eq!(
        durable_quota_usage(root.path()),
        0,
        "temporary staging admission is returned before exit"
    );
    let (staging, store) = lease.backend.live_roots().expect("roots");
    assert_eq!(
        directory_file_usage(&staging.join("staging")).expect("staging usage"),
        (0, 0)
    );
    assert_eq!(
        directory_file_usage(&store.join("blobs")).expect("blob usage"),
        (0, 0)
    );
}

#[test]
fn managed_capture_rechecks_authority_health_when_existing_capacity_covers_write() {
    let root = tempfile::tempdir().expect("tempdir");
    let write_allowed = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let writer = configured_writer_with_validation_gate(
        root.path(),
        64,
        100,
        true,
        Some(Arc::clone(&write_allowed)),
    );
    let lease = ManagedArtifactStoreLeaseV1::acquire(writer, "poisoned-quota", "poisoned-session")
        .expect("lease");
    let mut capture = lease
        .backend
        .clone()
        .begin_process_capture(capture_config(32))
        .expect("capture");
    capture
        .write_stream(ToolOutputStreamV1::Stdout, b"1234", 32)
        .expect("warm capacity");
    let (staging, _) = lease.backend.live_roots().expect("live roots");
    let cached = lease
        .backend
        .capture_usage
        .lock()
        .expect("usage")
        .expect("warm accounting");
    assert_eq!(cached.staging_bytes, 4);
    assert_eq!(cached.capacity_bytes, 64);
    let records = durable_quota_record_count(root.path());
    let scans = lease
        .backend
        .inventory_scans
        .load(std::sync::atomic::Ordering::Relaxed);
    write_allowed.store(false, std::sync::atomic::Ordering::Release);
    assert!(
        capture
            .write_stream(ToolOutputStreamV1::Stdout, b"5", 32)
            .is_err(),
        "spare capacity cannot bypass authority health"
    );
    assert_eq!(
        directory_file_usage(&staging.join("staging")).expect("unchanged physical prefix"),
        (4, 1)
    );
    assert_eq!(
        durable_quota_usage(root.path()),
        64,
        "failed validation retains the existing charge"
    );
    assert_eq!(durable_quota_record_count(root.path()), records);
    assert_eq!(
        lease
            .backend
            .inventory_scans
            .load(std::sync::atomic::Ordering::Relaxed),
        scans,
        "health checks do not inventory storage"
    );
    write_allowed.store(true, std::sync::atomic::Ordering::Release);
    assert!(
        capture.finish().is_err(),
        "the rejected write prevents complete capture"
    );
    assert_eq!(durable_quota_usage(root.path()), 0);
}
