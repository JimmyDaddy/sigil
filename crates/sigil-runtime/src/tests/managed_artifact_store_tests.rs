use std::sync::Arc;

use sigil_kernel::managed_storage::ManagedStorageServiceV1;
use sigil_kernel::resource::{AuthorityGeneration, CanonicalHash};
use sigil_resource_authority::storage::{
    AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
};

use super::*;

fn writer(root: &Path) -> Arc<ManagedStorageWriterAdapterV1> {
    let mut table = AuthorityStorageGrantTableV1::new();
    let staging_grant = crate::managed_storage_writer::grant_for_channel(
        StorageWriterChannelV1::ArtifactStaging,
        0x81,
    );
    let store_grant = crate::managed_storage_writer::grant_for_channel(
        StorageWriterChannelV1::ArtifactStore,
        0x82,
    );
    table
        .register(staging_grant.clone())
        .expect("staging grant");
    table.register(store_grant.clone()).expect("store grant");
    let generation = AuthorityGeneration {
        epoch: 1,
        instance_hash: CanonicalHash::from_bytes([0x28; 32]),
    };
    let service: Arc<dyn ManagedStorageServiceV1> =
        Arc::new(AuthorityManagedStorageServiceV1::new(table, generation));
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
