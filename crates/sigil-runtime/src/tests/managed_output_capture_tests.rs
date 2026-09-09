use super::*;
use std::path::Path;
use std::time::Duration;

use crate::managed_artifact_store::ManagedArtifactStoreLeaseV1;
use crate::managed_storage_writer::{ManagedStorageWriterAdapterV1, StorageWriterChannelV1};
use sigil_kernel::managed_storage::ManagedStorageServiceV1;
use sigil_kernel::resource::{AuthorityGeneration, CanonicalHash};
use sigil_kernel::{
    ProcessStreamCaptureConfigV1, ToolSourceCompletenessV1, ToolStorageCompletenessV1,
};
use sigil_resource_authority::storage::{
    AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
};
use sigil_sandbox::managed::ManagedOutputCaptureSinkV1;

#[derive(Debug)]
struct CapturePause {
    writing: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    allow_write: Mutex<std::sync::mpsc::Receiver<()>>,
    cleaning: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    allow_cleanup: Mutex<std::sync::mpsc::Receiver<()>>,
}

#[derive(Debug)]
struct PausedArtifactBackend(Arc<CapturePause>);

struct PausedProcessCapture(Arc<CapturePause>);

impl sigil_kernel::ToolArtifactProcessCaptureBackendV1 for PausedProcessCapture {
    fn write_stream(&mut self, _: ToolOutputStreamV1, bytes: &[u8], _: u64) -> Result<(u64, bool)> {
        if let Some(writing) = self.0.writing.lock().expect("write signal").take() {
            let _ = writing.send(());
            self.0
                .allow_write
                .lock()
                .expect("write gate")
                .recv_timeout(Duration::from_secs(3))
                .expect("release blocked write");
        }
        Ok((bytes.len() as u64, false))
    }

    fn finish(self: Box<Self>) -> Result<sigil_kernel::ToolArtifactProcessCaptureSnapshotV1> {
        anyhow::bail!("fault fixture only exercises abandoned capture cleanup")
    }
}

impl Drop for PausedProcessCapture {
    fn drop(&mut self) {
        if let Some(cleaning) = self.0.cleaning.lock().expect("cleanup signal").take() {
            let _ = cleaning.send(());
            // A bounded fault fixture cannot strand the test runtime if an assertion fails.
            let _ = self
                .0
                .allow_cleanup
                .lock()
                .expect("cleanup gate")
                .recv_timeout(Duration::from_secs(3));
        }
    }
}

impl sigil_kernel::ToolArtifactStoreBackendV1 for PausedArtifactBackend {
    fn begin_process_capture(
        self: Arc<Self>,
        _: ProcessStreamCaptureConfigV1,
    ) -> Result<Box<dyn sigil_kernel::ToolArtifactProcessCaptureBackendV1>> {
        Ok(Box::new(PausedProcessCapture(Arc::clone(&self.0))))
    }
    fn publish_blob(&self, _: &str, _: &[u8]) -> Result<()> {
        anyhow::bail!("unused fault fixture operation")
    }
    fn publish_descriptor_manifest(
        &self,
        _: &sigil_kernel::ToolArtifactDescriptorV1,
    ) -> Result<()> {
        anyhow::bail!("unused fault fixture operation")
    }
    fn read_blob(&self, _: &str) -> Result<Vec<u8>> {
        anyhow::bail!("unused fault fixture operation")
    }
    fn resolve(
        &self,
        _: &sigil_kernel::ToolArtifactRefV1,
    ) -> Result<sigil_kernel::ToolArtifactDescriptorV1> {
        anyhow::bail!("unused fault fixture operation")
    }
    fn read_page(
        &self,
        _: &sigil_kernel::ToolArtifactRefV1,
        _: sigil_kernel::ToolArtifactSelectorV1,
    ) -> Result<sigil_kernel::ToolArtifactPageV1> {
        anyhow::bail!("unused fault fixture operation")
    }
    fn manifest_inventory(&self) -> Result<Vec<sigil_kernel::ToolArtifactManifestEntryV1>> {
        anyhow::bail!("unused fault fixture operation")
    }
    fn bind_source_event(&self, _: &sigil_kernel::ToolArtifactRefV1, _: &str) -> Result<()> {
        anyhow::bail!("unused fault fixture operation")
    }
    fn source_event_id(&self, _: &sigil_kernel::ToolArtifactRefV1) -> Result<String> {
        anyhow::bail!("unused fault fixture operation")
    }
    fn garbage_collect(
        &self,
        _: &sigil_kernel::ToolArtifactGcRootsV1,
        _: u64,
        _: u64,
    ) -> Result<sigil_kernel::ToolArtifactGcReportV1> {
        anyhow::bail!("unused fault fixture operation")
    }
    fn prune_garbage_trash(
        &self,
        _: u64,
        _: u64,
    ) -> Result<sigil_kernel::ToolArtifactTrashPruneReportV1> {
        anyhow::bail!("unused fault fixture operation")
    }
}

#[tokio::test]
async fn aborted_capture_wait_keeps_root_task_until_storage_cleanup_finishes() {
    let (writing_tx, writing_rx) = tokio::sync::oneshot::channel();
    let (cleaning_tx, cleaning_rx) = tokio::sync::oneshot::channel();
    let (allow_write_tx, allow_write_rx) = std::sync::mpsc::channel();
    let (allow_cleanup_tx, allow_cleanup_rx) = std::sync::mpsc::channel();
    let control = Arc::new(CapturePause {
        writing: Mutex::new(Some(writing_tx)),
        allow_write: Mutex::new(allow_write_rx),
        cleaning: Mutex::new(Some(cleaning_tx)),
        allow_cleanup: Mutex::new(allow_cleanup_rx),
    });
    let store = sigil_kernel::ToolArtifactStore::from_backend(
        "paused-capture".to_owned(),
        Arc::new(PausedArtifactBackend(control)),
    )
    .expect("fault capture store");
    let config = capture_config(1024);
    let sink = store
        .begin_policy_safe_capture(
            "paused-call",
            "shell",
            "text/plain",
            sigil_kernel::ToolArtifactEncoding::Utf8,
            sigil_kernel::ToolArtifactSensitivity::Ordinary,
        )
        .begin_process_capture(config)
        .expect("process capture");
    let owner = sigil_kernel::RunCancellationOwner::new();
    let handle = owner.handle();
    let bridge = Arc::new(
        ManagedOutputCaptureBridge::new(ExecutionCaptureHandle { sink, config }, Some(&handle))
            .expect("owned capture"),
    );
    bridge
        .write_chunk(ManagedProcessOutputChannelV1::Stdout, b"output")
        .expect("reader enqueue");
    let pending_bridge = Arc::clone(&bridge);
    let waiting = tokio::spawn(async move { pending_bridge.finish().await });
    tokio::time::timeout(Duration::from_secs(2), writing_rx)
        .await
        .expect("writer started")
        .expect("write signal");
    owner.request_cancel();
    waiting.abort();
    assert!(waiting.await.expect_err("await cancelled").is_cancelled());
    assert_eq!(handle.active_tasks(), 1, "blocking write remains owned");
    allow_write_tx.send(()).expect("release writer");
    tokio::time::timeout(Duration::from_secs(2), cleaning_rx)
        .await
        .expect("cleanup started")
        .expect("cleanup signal");
    assert_eq!(
        handle.active_tasks(),
        1,
        "abandoned output cleanup remains owned"
    );
    assert_eq!(
        owner.wait_for_quiescence(Duration::from_millis(10)).await,
        sigil_kernel::RunQuiescenceOutcome::TimedOut {
            active_effects: 0,
            active_tasks: 1
        }
    );
    allow_cleanup_tx.send(()).expect("release cleanup");
    assert_eq!(
        owner.wait_for_quiescence(Duration::from_secs(2)).await,
        sigil_kernel::RunQuiescenceOutcome::Quiescent
    );
}

fn capture_writer(root: &Path) -> Arc<ManagedStorageWriterAdapterV1> {
    let generation = AuthorityGeneration {
        epoch: 1,
        instance_hash: CanonicalHash::from_bytes([0x28; 32]),
    };
    let cutover_manifest_hash = CanonicalHash::from_bytes([0x84; 32]);
    let mut table = AuthorityStorageGrantTableV1::new();
    let staging_grant = crate::managed_storage_writer::grant_for_channel_with_context(
        StorageWriterChannelV1::ArtifactStaging,
        0x81,
        generation,
        cutover_manifest_hash,
    );
    let store_grant = crate::managed_storage_writer::grant_for_channel_with_context(
        StorageWriterChannelV1::ArtifactStore,
        0x82,
        generation,
        cutover_manifest_hash,
    );
    table
        .register(staging_grant.clone())
        .expect("staging grant");
    table.register(store_grant.clone()).expect("store grant");
    let service: Arc<dyn ManagedStorageServiceV1> = Arc::new(
        AuthorityManagedStorageServiceV1::new_with_state_root(table, generation, root)
            .expect("durable quota authority"),
    );
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

fn capture_config(limit: u64) -> ProcessStreamCaptureConfigV1 {
    ProcessStreamCaptureConfigV1 {
        stream_layout: sigil_kernel::ToolOutputStreamLayoutV1::SeparatePipesNoCrossStreamOrder,
        preview_limit_bytes_per_stream: 64 * 1024,
        artifact_payload_limit_bytes_combined: limit,
        artifact_reservation_stdout_bytes: limit / 2,
        artifact_reservation_stderr_bytes: limit - limit / 2,
        artifact_staging_limit_bytes_per_stream: limit,
        observed_limit_bytes_combined: 128 * 1024 * 1024,
    }
}

fn capture_handle(lease: &ManagedArtifactStoreLeaseV1, limit: u64) -> ExecutionCaptureHandle {
    let config = capture_config(limit);
    let sink = lease
        .store()
        .begin_policy_safe_capture(
            "capture-call",
            "shell",
            "text/plain",
            sigil_kernel::ToolArtifactEncoding::Utf8,
            sigil_kernel::ToolArtifactSensitivity::Ordinary,
        )
        .begin_process_capture(config)
        .expect("managed staging");
    ExecutionCaptureHandle { sink, config }
}

#[tokio::test]
async fn buffered_capture_keeps_discarded_observations_after_physical_snapshot() {
    let root = tempfile::tempdir().expect("isolated storage");
    let lease = ManagedArtifactStoreLeaseV1::acquire(
        capture_writer(root.path()),
        "capture-buffer",
        "capture-session",
    )
    .expect("capture lease");
    let owner = sigil_kernel::RunCancellationOwner::new();
    let handle = owner.handle();
    let bridge = ManagedOutputCaptureBridge::new(capture_handle(&lease, 512), Some(&handle))
        .expect("capture bridge");
    for _ in 0..100 {
        bridge
            .write_chunk(ManagedProcessOutputChannelV1::Stdout, &[b'x'; 4096])
            .expect("enqueue");
    }
    bridge
        .write_chunk(ManagedProcessOutputChannelV1::Stderr, b"end")
        .expect("stderr");
    {
        let pending = bridge.pending.lock().expect("buffer lock");
        let pending = pending.as_ref().expect("pending capture");
        assert_eq!(pending.stdout.retained_bytes, 512);
        assert_eq!(
            pending
                .stdout
                .chunks
                .iter()
                .map(Vec::capacity)
                .sum::<usize>(),
            512
        );
        assert_eq!(pending.stdout.observed_bytes, 409_600);
    }
    assert_eq!(handle.active_tasks(), 1);
    owner.request_cancel();
    let capture = bridge.finish().await.expect("capture storage task");
    assert_eq!(handle.active_tasks(), 0);
    let (_, segments, completeness) = capture
        .sink
        .finish_process_capture(409_603, 0, ToolSourceCompletenessV1::Complete)
        .expect("artifact settlement");
    assert_eq!(segments[0].observed_bytes, 409_600);
    assert_eq!(segments[1].observed_bytes, 3);
    assert_eq!(
        segments[0].storage,
        ToolStorageCompletenessV1::TruncatedAtLimit
    );
    assert_eq!(completeness.source, ToolSourceCompletenessV1::Complete);
    assert!(
        bridge
            .write_chunk(ManagedProcessOutputChannelV1::Stdout, b"late")
            .is_err()
    );
    lease.finalize().expect("finalize leases");
}

#[cfg(unix)]
#[tokio::test]
async fn managed_capture_preserves_ten_megabytes_and_final_suffix() {
    use sigil_tools_builtin::ManagedCommandExecutionPortV1;
    let root = tempfile::tempdir().expect("isolated workspace");
    let temp = tempfile::tempdir().expect("isolated execution temp");
    let storage = tempfile::tempdir().expect("isolated artifact storage");
    let lease = ManagedArtifactStoreLeaseV1::acquire(
        capture_writer(storage.path()),
        "capture-large",
        "capture-large-session",
    )
    .expect("capture lease");
    let route = RuntimeManagedCommandExecutionRouteV1::new(
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new()),
        temp.path().to_path_buf(),
    )
    .with_process_inventory(Arc::new(
        sigil_resource_authority::InMemoryAuthorityProcessInventoryV1::default(),
    ));
    let size = 10 * 1024 * 1024;
    let suffix = b"THE-END\n";
    let command = format!(
        "/usr/bin/head -c {} /dev/zero | /usr/bin/tr '\\0' x; printf 'THE-END\\n'",
        size - suffix.len()
    );
    let receipt = route
        .execute_with_cancellation(
            ExecutionRequest {
                program: "/bin/sh".to_owned(),
                args: vec!["-c".to_owned(), command],
                cwd: root.path().to_path_buf(),
                env: BTreeMap::new(),
                environment_policy: sigil_kernel::ProcessEnvironmentPolicy::default(),
                timeout_ms: Some(30_000),
                timeout_secs: 30,
                cpu_time_ms: None,
                memory_limit_bytes: None,
                process_count_limit: None,
                capture: Some(capture_handle(
                    &lease,
                    sigil_kernel::session::TOOL_ARTIFACT_MAX_BYTES as u64,
                )),
            },
            None,
        )
        .await
        .expect("managed execution");
    assert_eq!(receipt.exit_code, Some(0));
    assert_eq!(receipt.output.combined_total_bytes, size as u64);
    let capture = receipt.capture.expect("completed capture");
    assert_eq!(capture.source, ToolSourceCompletenessV1::Complete);
    assert!(!capture.reader_failed);
    let (descriptor, _, completeness) = capture
        .sink
        .finish_process_capture(capture.observed_bytes, 0, capture.source)
        .expect("publish artifact");
    assert_eq!(completeness.storage, ToolStorageCompletenessV1::Complete);
    let body = lease
        .store()
        .read_all(&descriptor)
        .expect("read immutable artifact");
    assert_eq!(body.len(), size);
    assert!(body.ends_with(suffix));
    assert!(body[..size - suffix.len()].iter().all(|byte| *byte == b'x'));
    lease.finalize().expect("finalize leases");
}
