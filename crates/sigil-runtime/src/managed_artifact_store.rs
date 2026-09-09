//! Authority-admitted artifact physical layer for the current-schema runtime route.
//!
//! The kernel receives only `ToolArtifactStoreBackendV1`.  This module owns the two physical
//! leases, their filesystem paths, the namespace liveness checks, and the bounded staging files.
//! It is deliberately not exposed as a generic path writer to callers.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use sigil_kernel::session::{
    ProcessStreamCaptureConfigV1, TOOL_ARTIFACT_MAX_BYTES, TOOL_ARTIFACT_SESSION_BUDGET_BYTES,
    ToolArtifactDescriptorV1, ToolArtifactGcReportV1, ToolArtifactGcRootsV1,
    ToolArtifactManifestEntryV1, ToolArtifactPageV1, ToolArtifactProcessCaptureBackendV1,
    ToolArtifactProcessCaptureSnapshotV1, ToolArtifactRefV1, ToolArtifactRetireFrontierV1,
    ToolArtifactSelectorV1, ToolArtifactStore, ToolArtifactStoreBackendV1,
    ToolArtifactTrashPruneReportV1, ToolOutputStreamV1,
};

use crate::managed_storage_writer::{
    ManagedStorageWriterAdapterV1, ManagedStorageWriterLeaseV1, StorageWriterChannelV1,
};

/// The paired ArtifactStaging/ArtifactStore lease and its pathless kernel facade.
pub struct ManagedArtifactStoreLeaseV1 {
    backend: Arc<ManagedArtifactStoreBackendV1>,
    store: ToolArtifactStore,
}

impl std::fmt::Debug for ManagedArtifactStoreLeaseV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedArtifactStoreLeaseV1")
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl ManagedArtifactStoreLeaseV1 {
    pub fn acquire(
        writer: Arc<ManagedStorageWriterAdapterV1>,
        key: &str,
        session_scope_id: &str,
    ) -> Result<Self> {
        Self::acquire_with_session_path(writer, key, session_scope_id, PathBuf::new())
    }

    pub fn acquire_with_session_path(
        writer: Arc<ManagedStorageWriterAdapterV1>,
        key: &str,
        session_scope_id: &str,
        session_log_path: PathBuf,
    ) -> Result<Self> {
        let staging_lease = writer
            .acquire_named(StorageWriterChannelV1::ArtifactStaging, key)
            .map_err(|error| {
                anyhow::anyhow!("managed artifact-staging admission failed: {error}")
            })?;
        let store_lease = match writer.acquire_named(StorageWriterChannelV1::ArtifactStore, key) {
            Ok(lease) => lease,
            Err(error) => {
                let _ = writer.finalize(staging_lease);
                return Err(anyhow::anyhow!(
                    "managed artifact-store admission failed: {error}"
                ));
            }
        };
        let backend = Arc::new(ManagedArtifactStoreBackendV1::new(
            writer,
            staging_lease,
            store_lease,
        ));
        let store = ToolArtifactStore::from_backend_with_session_path(
            session_scope_id.to_owned(),
            session_log_path,
            backend.clone(),
        )?;
        Ok(Self { backend, store })
    }

    #[must_use]
    pub fn store(&self) -> ToolArtifactStore {
        self.store.clone()
    }

    #[must_use]
    pub fn writer(&self) -> Arc<ManagedStorageWriterAdapterV1> {
        Arc::clone(&self.backend.writer)
    }

    pub fn finalize(self) -> Result<()> {
        self.backend.finalize()
    }
}

impl Drop for ManagedArtifactStoreLeaseV1 {
    fn drop(&mut self) {
        if let Err(error) = self.backend.finalize() {
            tracing::error!(%error, "failed to finalize managed artifact namespaces");
        }
    }
}

#[derive(Debug)]
struct ManagedArtifactStoreBackendV1 {
    writer: Arc<ManagedStorageWriterAdapterV1>,
    staging_lease: Mutex<Option<ManagedStorageWriterLeaseV1>>,
    store_lease: Mutex<Option<ManagedStorageWriterLeaseV1>>,
    operation_lock: Mutex<()>,
    // Protected by operation_lock across quota admission and the corresponding physical write.
    capture_usage: Mutex<Option<ManagedArtifactCaptureUsage>>,
    #[cfg(test)]
    inventory_scans: std::sync::atomic::AtomicUsize,
}

#[derive(Debug, Clone, Copy)]
struct ManagedArtifactCaptureUsage {
    staging_bytes: u64,
    staging_files: u64,
    capacity_bytes: u64,
    capacity_entries: u64,
}

const ARTIFACT_CAPTURE_CAPACITY_BATCH_BYTES: u64 = 1024 * 1024;

impl ManagedArtifactStoreBackendV1 {
    fn new(
        writer: Arc<ManagedStorageWriterAdapterV1>,
        staging_lease: ManagedStorageWriterLeaseV1,
        store_lease: ManagedStorageWriterLeaseV1,
    ) -> Self {
        Self {
            writer,
            staging_lease: Mutex::new(Some(staging_lease)),
            store_lease: Mutex::new(Some(store_lease)),
            operation_lock: Mutex::new(()),
            capture_usage: Mutex::new(None),
            #[cfg(test)]
            inventory_scans: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn reconcile_quota(
        &self,
        staging_root: &Path,
        store_root: &Path,
        extra_staging_bytes: u64,
        extra_store_bytes: u64,
    ) -> Result<()> {
        let mut usage = self
            .capture_usage
            .lock()
            .map_err(|_| anyhow::anyhow!("managed artifact capture accounting is poisoned"))?;
        *usage = None;
        #[cfg(test)]
        self.inventory_scans
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (staging_bytes, staging_files) = directory_file_usage(&staging_root.join("staging"))?;
        let (store_bytes, store_files) = directory_file_usage(&store_root.join("blobs"))?;
        let staging_entries = staging_files
            .saturating_add(u64::from(extra_staging_bytes > 0))
            .max(1);
        let store_entries = store_files
            .saturating_add(u64::from(extra_store_bytes > 0))
            .max(1);
        let staging_guard = self
            .staging_lease
            .lock()
            .map_err(|_| anyhow::anyhow!("managed artifact staging lease is poisoned"))?;
        let staging = staging_guard
            .as_ref()
            .context("managed artifact staging lease is closed")?;
        let store_guard = self
            .store_lease
            .lock()
            .map_err(|_| anyhow::anyhow!("managed artifact store lease is poisoned"))?;
        let store = store_guard
            .as_ref()
            .context("managed artifact store lease is closed")?;
        self.writer
            .reconcile_artifact_quota(
                staging,
                staging_bytes.saturating_add(extra_staging_bytes),
                staging_entries,
                store,
                store_bytes.saturating_add(extra_store_bytes),
                store_entries,
            )
            .context("managed artifact quota reconcile failed")?;
        if extra_staging_bytes == 0 && extra_store_bytes == 0 {
            *usage = Some(ManagedArtifactCaptureUsage {
                staging_bytes,
                staging_files,
                capacity_bytes: staging_bytes,
                capacity_entries: staging_entries,
            });
        }
        Ok(())
    }

    fn ensure_capture_accounting(&self, staging_root: &Path, store_root: &Path) -> Result<()> {
        let initialized = self
            .capture_usage
            .lock()
            .map_err(|_| anyhow::anyhow!("managed artifact capture accounting is poisoned"))?
            .is_some();
        if !initialized {
            self.reconcile_quota(staging_root, store_root, 0, 0)?;
        }
        Ok(())
    }

    /// The caller holds operation_lock until the admitted bytes are written or accounted as
    /// uncertain. Capacity is shared by every capture in this namespace, not per stream/call.
    fn reserve_capture_write(
        &self,
        bytes: u64,
        creates_file: bool,
        remaining_capture_bytes: u64,
    ) -> Result<()> {
        let mut usage_guard = self
            .capture_usage
            .lock()
            .map_err(|_| anyhow::anyhow!("managed artifact capture accounting is poisoned"))?;
        let usage = usage_guard
            .as_mut()
            .context("managed artifact capture accounting is unavailable")?;
        let minimum = usage
            .staging_bytes
            .checked_add(bytes)
            .context("managed artifact staging byte count overflow")?;
        let files = usage
            .staging_files
            .checked_add(u64::from(creates_file))
            .context("managed artifact staging entry count overflow")?;
        let entries = files.max(1);
        if minimum > usage.capacity_bytes || entries > usage.capacity_entries {
            let preferred = usage.staging_bytes.saturating_add(
                bytes.max(remaining_capture_bytes.min(ARTIFACT_CAPTURE_CAPACITY_BATCH_BYTES)),
            );
            let staging_guard = self
                .staging_lease
                .lock()
                .map_err(|_| anyhow::anyhow!("managed artifact staging lease is poisoned"))?;
            let staging = staging_guard
                .as_ref()
                .context("managed artifact staging lease is closed")?;
            let capacity = self
                .writer
                .reserve_artifact_staging_capacity(staging, minimum, preferred, entries)?;
            anyhow::ensure!(
                capacity >= minimum && capacity <= preferred,
                "managed artifact authority returned an invalid capacity"
            );
            usage.capacity_bytes = capacity;
            usage.capacity_entries = entries;
        }
        // Charge the entire admitted write pessimistically. Partial I/O invalidates this cache
        // before any later operation; no failed write can make spare capacity appear larger.
        usage.staging_bytes = minimum;
        usage.staging_files = files;
        Ok(())
    }

    fn invalidate_capture_accounting(&self) {
        if let Ok(mut usage) = self.capture_usage.lock() {
            *usage = None;
        }
    }

    fn live_roots(&self) -> Result<(PathBuf, PathBuf)> {
        let staging = self
            .staging_lease
            .lock()
            .map_err(|_| anyhow::anyhow!("managed artifact staging lease is poisoned"))?;
        let store = self
            .store_lease
            .lock()
            .map_err(|_| anyhow::anyhow!("managed artifact store lease is poisoned"))?;
        let staging = staging
            .as_ref()
            .context("managed artifact staging lease is closed")?;
        let store = store
            .as_ref()
            .context("managed artifact store lease is closed")?;
        self.writer
            .validate_artifact_lease(staging)
            .map_err(|error| anyhow::anyhow!("managed artifact staging lease rejected: {error}"))?;
        self.writer
            .validate_artifact_lease(store)
            .map_err(|error| anyhow::anyhow!("managed artifact store lease rejected: {error}"))?;
        Ok((staging.path().to_path_buf(), store.path().to_path_buf()))
    }

    fn finalize(&self) -> Result<()> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("managed artifact operation lock is poisoned"))?;
        let store = self
            .store_lease
            .lock()
            .map_err(|_| anyhow::anyhow!("managed artifact store lease is poisoned"))?
            .take();
        let staging = self
            .staging_lease
            .lock()
            .map_err(|_| anyhow::anyhow!("managed artifact staging lease is poisoned"))?
            .take();
        let mut first_error = None;
        if let Some(lease) = store
            && let Err(error) = self.writer.finalize(lease)
        {
            first_error = Some(anyhow::anyhow!(
                "managed artifact-store finalize failed: {error}"
            ));
        }
        if let Some(lease) = staging
            && let Err(error) = self.writer.finalize(lease)
            && first_error.is_none()
        {
            first_error = Some(anyhow::anyhow!(
                "managed artifact-staging finalize failed: {error}"
            ));
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn ensure_layout(staging_root: &Path, store_root: &Path) -> Result<()> {
        create_private_dir(store_root)?;
        create_private_dir(&store_root.join("blobs"))?;
        create_private_dir(&store_root.join("refs"))?;
        create_private_dir(&store_root.join("locks"))?;
        create_private_dir(&staging_root.join("staging"))?;
        Ok(())
    }

    fn with_mutation_lock(&self) -> Result<(std::sync::MutexGuard<'_, ()>, PathBuf, PathBuf)> {
        let operation = self
            .operation_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("managed artifact operation lock is poisoned"))?;
        let (staging, store) = self.live_roots()?;
        Self::ensure_layout(&staging, &store)?;
        Ok((operation, staging, store))
    }

    fn usage_lock(store_root: &Path) -> Result<File> {
        let path = store_root.join("usage.lock");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        file.lock_exclusive()
            .with_context(|| format!("failed to lock {}", path.display()))?;
        Ok(file)
    }

    fn current_usage(store_root: &Path) -> Result<u64> {
        directory_file_bytes(&store_root.join("blobs"))
    }

    fn publish_blob_locked(
        staging_root: &Path,
        store_root: &Path,
        hash: &str,
        bytes: &[u8],
    ) -> Result<()> {
        let blob_path = blob_path(store_root, hash)?;
        if let Ok(existing) = fs::symlink_metadata(&blob_path) {
            if !existing.is_file() || existing.file_type().is_symlink() {
                bail!("managed artifact blob is not a regular file");
            }
            if hash_bytes(&fs::read(&blob_path)?) != hash {
                bail!("existing managed artifact blob hash mismatch");
            }
            return Ok(());
        }
        let usage_lock = Self::usage_lock(store_root)?;
        let current = Self::current_usage(store_root)?;
        if current.saturating_add(bytes.len() as u64) > TOOL_ARTIFACT_SESSION_BUDGET_BYTES {
            bail!("managed artifact session budget exceeded");
        }
        let _keep_lock = usage_lock;
        let staging_path = staging_root
            .join("staging")
            .join(format!("{}.part", Uuid::new_v4().simple()));
        let mut staging = create_private_file(&staging_path)?;
        staging.write_all(bytes)?;
        staging.sync_all()?;
        drop(staging);
        let blob_parent = blob_path
            .parent()
            .context("managed artifact blob parent is missing")?;
        create_private_dir(blob_parent)?;
        match fs::hard_link(&staging_path, &blob_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if hash_bytes(&fs::read(&blob_path)?) != hash {
                    let _ = fs::remove_file(&staging_path);
                    bail!("racing managed artifact blob hash mismatch");
                }
            }
            Err(error) => {
                let _ = fs::remove_file(&staging_path);
                return Err(error).context("failed to publish managed artifact blob");
            }
        }
        let _ = fs::remove_file(staging_path);
        sync_parent(blob_parent)
    }
}

impl ToolArtifactStoreBackendV1 for ManagedArtifactStoreBackendV1 {
    fn publish_blob(&self, content_sha256: &str, bytes: &[u8]) -> Result<()> {
        let (_operation, staging, store) = self.with_mutation_lock()?;
        let blob_path = blob_path(&store, content_sha256)?;
        let extra_store_bytes = match fs::symlink_metadata(&blob_path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => 0,
            Ok(_) => bytes.len() as u64,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => bytes.len() as u64,
            Err(error) => return Err(error.into()),
        };
        let publication = (|| -> Result<()> {
            self.reconcile_quota(&staging, &store, extra_store_bytes, extra_store_bytes)?;
            Self::publish_blob_locked(&staging, &store, content_sha256, bytes)
        })();
        if let Err(error) = publication {
            // Return unused admission after a denied/failed publication. A remaining partial
            // staging file is measured and stays charged; reconciliation failure stays
            // conservative and leaves the cache invalid for the next admitted operation.
            if let Err(cleanup_error) = self.reconcile_quota(&staging, &store, 0, 0) {
                tracing::error!(%cleanup_error, "failed to reconcile rejected artifact publication");
            }
            return Err(error);
        }
        self.reconcile_quota(&staging, &store, 0, 0)
    }

    fn publish_descriptor_manifest(&self, descriptor: &ToolArtifactDescriptorV1) -> Result<()> {
        let (_operation, staging, store) = self.with_mutation_lock()?;
        let refs = store.join("refs");
        create_private_dir(&refs)?;
        let path = refs.join(format!("{}.json", descriptor.artifact_ref.artifact_id));
        let bytes = serde_json::to_vec(descriptor)?;
        publish_noclobber(&refs, &path, &bytes)?;
        self.reconcile_quota(&staging, &store, 0, 0)
    }

    fn read_blob(&self, content_sha256: &str) -> Result<Vec<u8>> {
        let (_operation, _staging, store) = self.with_mutation_lock()?;
        read_blob_at(&store, content_sha256)
    }

    fn read_artifact(
        &self,
        artifact_ref: &ToolArtifactRefV1,
        content_sha256: &str,
    ) -> Result<Vec<u8>> {
        artifact_ref.validate()?;
        let (_operation, _staging, store) = self.with_mutation_lock()?;
        let lock_path = store
            .join("locks")
            .join(format!("{}.lock", artifact_ref.artifact_id));
        let lock = open_private_lock(&lock_path)?;
        lock.lock_shared()
            .context("failed to acquire managed artifact read lease")?;
        read_blob_at(&store, content_sha256)
    }

    fn availability(
        &self,
        artifact_ref: &ToolArtifactRefV1,
        content_sha256: &str,
    ) -> sigil_kernel::session::ToolArtifactAvailability {
        use sigil_kernel::session::ToolArtifactAvailability;

        let outcome = (|| -> Result<Vec<u8>> {
            artifact_ref.validate()?;
            let (_operation, _staging, store) = self.with_mutation_lock()?;
            let lock_path = store
                .join("locks")
                .join(format!("{}.lock", artifact_ref.artifact_id));
            let lock = open_private_lock(&lock_path)?;
            lock.lock_shared()
                .context("failed to acquire managed artifact availability lease")?;
            read_blob_bytes_at(&store, content_sha256)
        })();
        match outcome {
            Ok(bytes) if hash_bytes(&bytes) == content_sha256 => {
                ToolArtifactAvailability::Available
            }
            Ok(_) => ToolArtifactAvailability::HashMismatch,
            Err(_) => ToolArtifactAvailability::Missing,
        }
    }

    fn resolve(&self, artifact_ref: &ToolArtifactRefV1) -> Result<ToolArtifactDescriptorV1> {
        artifact_ref.validate()?;
        let (_operation, _staging, store) = self.with_mutation_lock()?;
        let path = store
            .join("refs")
            .join(format!("{}.json", artifact_ref.artifact_id));
        let bytes = read_bounded(&path, 16 * 1024)?;
        let descriptor = serde_json::from_slice(&bytes)?;
        Ok(descriptor)
    }

    fn read_page(
        &self,
        artifact_ref: &ToolArtifactRefV1,
        selector: ToolArtifactSelectorV1,
    ) -> Result<ToolArtifactPageV1> {
        artifact_ref.validate()?;
        let (_operation, _staging, store) = self.with_mutation_lock()?;
        let lock_path = store
            .join("locks")
            .join(format!("{}.lock", artifact_ref.artifact_id));
        let lock = open_private_lock(&lock_path)?;
        lock.lock_shared()
            .context("failed to acquire managed artifact read lease")?;
        let manifest_path = store
            .join("refs")
            .join(format!("{}.json", artifact_ref.artifact_id));
        let descriptor: ToolArtifactDescriptorV1 =
            serde_json::from_slice(&read_bounded(&manifest_path, 16 * 1024)?)?;
        descriptor.validate()?;
        let bytes = read_blob_at(&store, &descriptor.content_sha256)?;
        ToolArtifactStore::tool_artifact_page_from_bytes(
            &descriptor,
            artifact_ref,
            selector,
            &bytes,
        )
    }

    fn manifest_inventory(&self) -> Result<Vec<ToolArtifactManifestEntryV1>> {
        let (_operation, _staging, store) = self.with_mutation_lock()?;
        let refs = store.join("refs");
        let mut entries = Vec::new();
        let directory = match fs::read_dir(&refs) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
            Err(error) => return Err(error).context("failed to read managed artifact refs"),
        };
        for entry in directory {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > 16 * 1024
            {
                bail!("managed artifact manifest is unsafe");
            }
            let descriptor: ToolArtifactDescriptorV1 =
                serde_json::from_slice(&read_no_follow(&path)?)?;
            entries.push(ToolArtifactManifestEntryV1 {
                descriptor,
                manifest_modified_at_unix_ms: modified_at(&metadata),
            });
        }
        entries.sort_by(|left, right| {
            left.descriptor
                .artifact_ref
                .cmp(&right.descriptor.artifact_ref)
        });
        Ok(entries)
    }

    fn bind_source_event(
        &self,
        artifact_ref: &ToolArtifactRefV1,
        source_event_id: &str,
    ) -> Result<()> {
        let (_operation, _staging, store) = self.with_mutation_lock()?;
        let refs = store.join("refs");
        create_private_dir(&refs)?;
        let path = refs.join(format!("{}.event", artifact_ref.artifact_id));
        publish_noclobber(&refs, &path, source_event_id.as_bytes())
    }

    fn source_event_id(&self, artifact_ref: &ToolArtifactRefV1) -> Result<String> {
        let (_operation, _staging, store) = self.with_mutation_lock()?;
        let path = store
            .join("refs")
            .join(format!("{}.event", artifact_ref.artifact_id));
        let value = String::from_utf8(read_bounded(&path, 256)?)?;
        if value.trim().is_empty() {
            bail!("managed artifact source event is empty");
        }
        Ok(value)
    }

    fn garbage_collect(
        &self,
        _roots: &ToolArtifactGcRootsV1,
        _now_unix_ms: u64,
        _orphan_grace_ms: u64,
    ) -> Result<ToolArtifactGcReportV1> {
        bail!("managed artifact retirement requires the authority retire frontier")
    }

    fn garbage_collect_with_retire_frontier(
        &self,
        roots: &ToolArtifactGcRootsV1,
        now_unix_ms: u64,
        orphan_grace_ms: u64,
        retire_frontier: ToolArtifactRetireFrontierV1,
    ) -> Result<ToolArtifactGcReportV1> {
        roots.validate()?;
        if orphan_grace_ms < sigil_kernel::session::TOOL_ARTIFACT_ORPHAN_GRACE_MS {
            bail!("tool artifact orphan grace must be at least 24 hours");
        }
        let (_operation, staging_root, store_root) = self.with_mutation_lock()?;
        let inventory = manifest_inventory_at(&store_root)?;
        let mut candidates = Vec::new();
        let mut retained_manifests = 0usize;
        for entry in &inventory {
            let descriptor = &entry.descriptor;
            let protected = roots.contains(&descriptor.artifact_ref)
                || descriptor.retention_class == sigil_kernel::ToolArtifactRetentionClass::Pinned;
            let grace_elapsed =
                now_unix_ms.saturating_sub(entry.manifest_modified_at_unix_ms) >= orphan_grace_ms;
            if protected || !grace_elapsed {
                retained_manifests = retained_manifests.saturating_add(1);
            } else {
                candidates.push(entry.clone());
            }
        }
        let selected_bytes = candidates.iter().fold(0_u64, |total, entry| {
            total.saturating_add(entry.descriptor.persisted_bytes)
        });
        let selected_refs = candidates
            .iter()
            .map(|entry| entry.descriptor.artifact_ref.clone())
            .collect::<Vec<_>>();
        let expected_refs_hash = artifact_refs_hash(&selected_refs);
        if retire_frontier.selected_count != candidates.len() as u64
            || retire_frontier.selected_bytes != selected_bytes
            || retire_frontier.selected_refs_hash != expected_refs_hash
        {
            bail!("managed artifact retire frontier does not match the current manifest selection");
        }
        let tombstone_id = format!("tool-artifact-gc-{}", Uuid::new_v4().simple());
        if candidates.is_empty() {
            self.reconcile_quota(&staging_root, &store_root, 0, 0)?;
            return Ok(ToolArtifactGcReportV1 {
                tombstone_id,
                scanned_manifests: inventory.len(),
                retained_manifests,
                tombstoned_manifests: 0,
                tombstoned_blobs: 0,
                tombstoned_orphan_blobs: 0,
                tombstoned_staging_files: 0,
                tombstoned_bytes: 0,
                skipped_active_reads: 0,
                tombstoned_refs: Vec::new(),
            });
        }
        let mut token = self
            .writer
            .authorize_artifact_retirement(retire_frontier)
            .map_err(|error| {
                anyhow::anyhow!("managed artifact retire authorization failed: {error}")
            })?;
        self.writer
            .consume_artifact_retirement(&mut token)
            .map_err(|error| anyhow::anyhow!("managed artifact retire claim failed: {error}"))?;

        let trash_root = store_root.join("trash").join(&tombstone_id);
        let trash_refs = trash_root.join("refs");
        let trash_blobs = trash_root.join("blobs");
        let staging_trash_root = staging_root.join("trash");
        let trash_staging = staging_trash_root.join(&tombstone_id);
        create_private_dir(&store_root.join("trash"))?;
        create_private_dir(&staging_trash_root)?;
        create_private_dir(&trash_root)?;
        create_private_dir(&trash_refs)?;
        create_private_dir(&trash_blobs)?;
        create_private_dir(&trash_staging)?;

        let mut tombstoned_manifests = 0usize;
        let mut tombstoned_refs = Vec::new();
        let mut skipped_active_reads = 0usize;
        for entry in candidates {
            let artifact_ref = &entry.descriptor.artifact_ref;
            let lock_path = store_root
                .join("locks")
                .join(format!("{}.lock", artifact_ref.artifact_id));
            let lock = open_private_lock(&lock_path)?;
            match lock.try_lock_exclusive() {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    skipped_active_reads = skipped_active_reads.saturating_add(1);
                    retained_manifests = retained_manifests.saturating_add(1);
                    continue;
                }
                Err(error) => return Err(error).context("failed to lock managed artifact for GC"),
            }
            let source = store_root
                .join("refs")
                .join(format!("{}.json", artifact_ref.artifact_id));
            let destination = trash_refs.join(
                source
                    .file_name()
                    .context("managed artifact manifest has no file name")?,
            );
            fs::rename(&source, &destination).with_context(|| {
                format!(
                    "failed to tombstone managed artifact {}",
                    artifact_ref.artifact_id
                )
            })?;
            let source_binding = store_root
                .join("refs")
                .join(format!("{}.event", artifact_ref.artifact_id));
            if fs::symlink_metadata(&source_binding).is_ok() {
                fs::rename(
                    &source_binding,
                    trash_refs.join(format!("{}.event", artifact_ref.artifact_id)),
                )?;
            }
            tombstoned_manifests = tombstoned_manifests.saturating_add(1);
            tombstoned_refs.push(artifact_ref.clone());
        }
        sync_parent(&store_root.join("refs"))?;
        sync_parent(&trash_refs)?;

        let live_hashes = manifest_inventory_at(&store_root)?
            .into_iter()
            .map(|entry| entry.descriptor.content_sha256)
            .collect::<std::collections::BTreeSet<_>>();
        let mut tombstoned_blobs = 0usize;
        let mut tombstoned_orphan_blobs = 0usize;
        let mut tombstoned_staging_files = 0usize;
        let mut tombstoned_bytes = 0_u64;
        for (source, bytes) in
            collect_unreferenced_blobs(&store_root, &live_hashes, now_unix_ms, orphan_grace_ms)?
        {
            let destination = trash_blobs.join(
                source
                    .file_name()
                    .context("managed artifact blob has no file name")?,
            );
            fs::rename(&source, &destination)?;
            tombstoned_blobs = tombstoned_blobs.saturating_add(1);
            tombstoned_orphan_blobs = tombstoned_orphan_blobs.saturating_add(1);
            tombstoned_bytes = tombstoned_bytes.saturating_add(bytes);
        }
        for (source, bytes) in
            collect_old_staging_files(&staging_root.join("staging"), now_unix_ms, orphan_grace_ms)?
        {
            let destination = trash_staging.join(
                source
                    .file_name()
                    .context("managed artifact staging file has no file name")?,
            );
            fs::rename(&source, &destination)?;
            tombstoned_staging_files = tombstoned_staging_files.saturating_add(1);
            tombstoned_bytes = tombstoned_bytes.saturating_add(bytes);
        }
        sync_parent(&trash_blobs)?;
        sync_parent(&trash_staging)?;
        sync_parent(&staging_trash_root)?;
        sync_parent(&trash_root)?;
        self.reconcile_quota(&staging_root, &store_root, 0, 0)?;
        Ok(ToolArtifactGcReportV1 {
            tombstone_id,
            scanned_manifests: inventory.len(),
            retained_manifests,
            tombstoned_manifests,
            tombstoned_blobs,
            tombstoned_orphan_blobs,
            tombstoned_staging_files,
            tombstoned_bytes,
            skipped_active_reads,
            tombstoned_refs,
        })
    }

    fn prune_garbage_trash(
        &self,
        now_unix_ms: u64,
        trash_grace_ms: u64,
    ) -> Result<ToolArtifactTrashPruneReportV1> {
        if trash_grace_ms < sigil_kernel::session::TOOL_ARTIFACT_ORPHAN_GRACE_MS {
            bail!("tool artifact trash grace must be at least 24 hours");
        }
        let (_operation, staging_root, store_root) = self.with_mutation_lock()?;
        let mut eligible = Vec::new();
        for root in [store_root.join("trash"), staging_root.join("trash")] {
            let entries = match fs::read_dir(&root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("failed to read managed artifact trash"),
            };
            for entry in entries {
                let path = entry?.path();
                let metadata = fs::symlink_metadata(&path)?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    bail!("managed artifact trash contains an unsafe entry");
                }
                if now_unix_ms.saturating_sub(modified_at(&metadata)) >= trash_grace_ms {
                    eligible.push(path);
                }
            }
        }
        if eligible.is_empty() {
            return Ok(ToolArtifactTrashPruneReportV1 {
                removed_tombstones: 0,
                removed_bytes: 0,
            });
        }
        let selected_bytes = eligible
            .iter()
            .map(|path| directory_file_bytes(path))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .sum();
        let mut token = self
            .writer
            .authorize_artifact_retirement(ToolArtifactRetireFrontierV1 {
                selected_refs_hash: artifact_labels_hash(&eligible),
                selected_count: eligible.len() as u64,
                selected_bytes,
                eligibility_frontier: now_unix_ms.max(1),
                policy_hash: canonical_sha256(
                    format!("artifact-trash-prune:{trash_grace_ms}").as_bytes(),
                ),
            })
            .map_err(|error| {
                anyhow::anyhow!("managed artifact trash authorization failed: {error}")
            })?;
        self.writer
            .consume_artifact_retirement(&mut token)
            .map_err(|error| anyhow::anyhow!("managed artifact trash claim failed: {error}"))?;
        let mut removed_bytes = 0_u64;
        for path in &eligible {
            removed_bytes = removed_bytes.saturating_add(directory_file_bytes(path)?);
            fs::remove_dir_all(path)?;
        }
        sync_parent(&store_root.join("trash"))?;
        sync_parent(&staging_root.join("trash"))?;
        self.reconcile_quota(&staging_root, &store_root, 0, 0)?;
        Ok(ToolArtifactTrashPruneReportV1 {
            removed_tombstones: eligible
                .iter()
                .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            removed_bytes,
        })
    }

    fn begin_process_capture(
        self: Arc<Self>,
        config: ProcessStreamCaptureConfigV1,
    ) -> Result<Box<dyn ToolArtifactProcessCaptureBackendV1>> {
        let staging = {
            let (_operation, staging, store) = self.with_mutation_lock()?;
            self.ensure_capture_accounting(&staging, &store)?;
            staging
        };
        let directory = staging.join("staging");
        Ok(Box::new(ManagedProcessCaptureBackend {
            owner: self,
            stdout_path: directory.join(format!("{}.stdout.part", Uuid::new_v4().simple())),
            stderr_path: directory.join(format!("{}.stderr.part", Uuid::new_v4().simple())),
            stdout: None,
            stderr: None,
            stdout_bytes: 0,
            stderr_bytes: 0,
            stdout_truncated: false,
            stderr_truncated: false,
            per_stream_limit: config.artifact_staging_limit_bytes_per_stream,
            storage_failed: false,
            settled: false,
        }))
    }
}

struct ManagedProcessCaptureBackend {
    owner: Arc<ManagedArtifactStoreBackendV1>,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    stdout: Option<File>,
    stderr: Option<File>,
    stdout_bytes: u64,
    stderr_bytes: u64,
    stdout_truncated: bool,
    stderr_truncated: bool,
    per_stream_limit: u64,
    storage_failed: bool,
    settled: bool,
}

impl std::fmt::Debug for ManagedProcessCaptureBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedProcessCaptureBackend")
            .field("stdout_bytes", &self.stdout_bytes)
            .field("stderr_bytes", &self.stderr_bytes)
            .finish_non_exhaustive()
    }
}

impl Drop for ManagedProcessCaptureBackend {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        if let Err(error) = self.remove_staging() {
            tracing::error!(%error, "failed to settle managed artifact staging quota");
        }
    }
}

impl ManagedProcessCaptureBackend {
    fn remove_staging(&mut self) -> Result<()> {
        let owner = Arc::clone(&self.owner);
        let (_operation, staging, store) = owner.with_mutation_lock()?;
        self.stdout.take();
        self.stderr.take();
        // Keep the conservative reservation if cleanup fails; a later admitted inventory can
        // reconcile the remaining physical prefix, including after process death.
        owner.invalidate_capture_accounting();
        remove_capture_file(&self.stdout_path)?;
        remove_capture_file(&self.stderr_path)?;
        sync_parent(&staging.join("staging"))?;
        owner.reconcile_quota(&staging, &store, 0, 0)?;
        self.settled = true;
        Ok(())
    }
}

impl ToolArtifactProcessCaptureBackendV1 for ManagedProcessCaptureBackend {
    fn write_stream(
        &mut self,
        stream: ToolOutputStreamV1,
        bytes: &[u8],
        limit: u64,
    ) -> Result<(u64, bool)> {
        if stream == ToolOutputStreamV1::Combined {
            return Ok((0, false));
        }
        let limit = limit.min(self.per_stream_limit);
        let remaining_capture_bytes = self
            .per_stream_limit
            .saturating_mul(2)
            .saturating_sub(self.stdout_bytes.min(self.per_stream_limit))
            .saturating_sub(self.stderr_bytes.min(self.per_stream_limit));
        let owner = Arc::clone(&self.owner);
        let _operation = owner
            .operation_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("managed artifact operation lock is poisoned"))?;
        let (file, observed, truncated, path) = match stream {
            ToolOutputStreamV1::Stdout => (
                &mut self.stdout,
                &mut self.stdout_bytes,
                &mut self.stdout_truncated,
                &self.stdout_path,
            ),
            ToolOutputStreamV1::Stderr => (
                &mut self.stderr,
                &mut self.stderr_bytes,
                &mut self.stderr_truncated,
                &self.stderr_path,
            ),
            ToolOutputStreamV1::Combined => return Ok((0, false)),
        };
        let before = *observed;
        *observed = observed.saturating_add(bytes.len() as u64);
        let allowed = limit.saturating_sub(before).min(bytes.len() as u64) as usize;
        *truncated |= allowed < bytes.len();
        if self.storage_failed {
            bail!("managed artifact capture storage is unavailable after an earlier write failure");
        }
        let written = (|| -> Result<()> {
            let (staging_root, store_root) = owner.live_roots()?;
            if allowed == 0 {
                return Ok(());
            }
            owner.ensure_capture_accounting(&staging_root, &store_root)?;
            owner.reserve_capture_write(allowed as u64, file.is_none(), remaining_capture_bytes)?;
            if file.is_none() {
                *file = Some(create_private_file(path)?);
            }
            file.as_mut()
                .context("managed capture file is unavailable")?
                .write_all(&bytes[..allowed])?;
            Ok(())
        })();
        if let Err(error) = written {
            self.storage_failed = true;
            owner.invalidate_capture_accounting();
            return Err(error);
        }
        Ok((*observed, *truncated))
    }

    fn finish(mut self: Box<Self>) -> Result<ToolArtifactProcessCaptureSnapshotV1> {
        let (stdout, stderr) = {
            let (_operation, _staging, _store) = self.owner.with_mutation_lock()?;
            if let Some(file) = self.stdout.as_mut() {
                file.sync_all()?;
            }
            if let Some(file) = self.stderr.as_mut() {
                file.sync_all()?;
            }
            (
                read_optional_file(&self.stdout_path)?,
                read_optional_file(&self.stderr_path)?,
            )
        };
        self.remove_staging()?;
        anyhow::ensure!(
            !self.storage_failed,
            "managed artifact capture cannot complete after a storage write failure"
        );
        Ok(ToolArtifactProcessCaptureSnapshotV1 {
            stdout_bytes: stdout,
            stderr_bytes: stderr,
            stdout_observed_bytes: self.stdout_bytes,
            stderr_observed_bytes: self.stderr_bytes,
            stdout_truncated: self.stdout_truncated,
            stderr_truncated: self.stderr_truncated,
        })
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

fn read_blob_at(store_root: &Path, content_sha256: &str) -> Result<Vec<u8>> {
    let bytes = read_blob_bytes_at(store_root, content_sha256)?;
    if hash_bytes(&bytes) != content_sha256 {
        bail!("managed artifact blob hash mismatch");
    }
    Ok(bytes)
}

fn read_blob_bytes_at(store_root: &Path, content_sha256: &str) -> Result<Vec<u8>> {
    let path = blob_path(store_root, content_sha256)?;
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > TOOL_ARTIFACT_MAX_BYTES as u64
    {
        bail!("managed artifact blob is not a bounded regular file");
    }
    read_no_follow(&path)
}

fn canonical_sha256(bytes: &[u8]) -> sigil_kernel::resource::CanonicalHash {
    sigil_kernel::resource::CanonicalHash::from_bytes(Sha256::digest(bytes).into())
}

fn artifact_refs_hash(refs: &[ToolArtifactRefV1]) -> sigil_kernel::resource::CanonicalHash {
    let mut refs = refs.to_vec();
    refs.sort();
    canonical_sha256(&serde_json::to_vec(&refs).unwrap_or_default())
}

fn artifact_labels_hash(paths: &[PathBuf]) -> sigil_kernel::resource::CanonicalHash {
    let labels = paths
        .iter()
        .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
        .collect::<Vec<_>>();
    canonical_sha256(&serde_json::to_vec(&labels).unwrap_or_default())
}

fn open_private_lock(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(path)
        .with_context(|| format!("failed to open managed artifact lock {}", path.display()))?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("managed artifact lock is not a regular file");
    }
    file.sync_data()?;
    Ok(file)
}

fn manifest_inventory_at(store_root: &Path) -> Result<Vec<ToolArtifactManifestEntryV1>> {
    let refs = store_root.join("refs");
    let directory = match fs::read_dir(&refs) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("failed to read managed artifact refs"),
    };
    let mut entries = Vec::new();
    for entry in directory {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 16 * 1024 {
            bail!("managed artifact manifest is unsafe");
        }
        let descriptor: ToolArtifactDescriptorV1 = serde_json::from_slice(&read_no_follow(&path)?)?;
        entries.push(ToolArtifactManifestEntryV1 {
            descriptor,
            manifest_modified_at_unix_ms: modified_at(&metadata),
        });
    }
    entries.sort_by(|left, right| {
        left.descriptor
            .artifact_ref
            .cmp(&right.descriptor.artifact_ref)
    });
    Ok(entries)
}

fn collect_unreferenced_blobs(
    store_root: &Path,
    live_hashes: &std::collections::BTreeSet<String>,
    now_unix_ms: u64,
    grace_ms: u64,
) -> Result<Vec<(PathBuf, u64)>> {
    let blobs = store_root.join("blobs");
    let mut candidates = Vec::new();
    let directories = match fs::read_dir(&blobs) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidates),
        Err(error) => return Err(error.into()),
    };
    for directory in directories {
        let directory = directory?.path();
        let metadata = fs::symlink_metadata(&directory)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("managed artifact blob prefix is unsafe");
        }
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("managed artifact blob is unsafe");
            }
            let bytes = read_no_follow(&path)?;
            let hash = hash_bytes(&bytes);
            if !live_hashes.contains(&hash)
                && now_unix_ms.saturating_sub(modified_at(&metadata)) >= grace_ms
            {
                candidates.push((path, metadata.len()));
            }
        }
    }
    Ok(candidates)
}

fn collect_old_staging_files(
    staging_root: &Path,
    now_unix_ms: u64,
    grace_ms: u64,
) -> Result<Vec<(PathBuf, u64)>> {
    let entries = match fs::read_dir(staging_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut candidates = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("managed artifact staging entry is unsafe");
        }
        if now_unix_ms.saturating_sub(modified_at(&metadata)) >= grace_ms {
            candidates.push((path, metadata.len()));
        }
    }
    Ok(candidates)
}

fn blob_path(root: &Path, hash: &str) -> Result<PathBuf> {
    let digest = hash
        .strip_prefix("sha256:")
        .context("managed artifact hash has unsupported format")?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("managed artifact hash is malformed");
    }
    Ok(root
        .join("blobs")
        .join(&digest[..2])
        .join(format!("{digest}.blob")))
}

fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn create_private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    Ok(options.open(path)?)
}

fn read_no_follow(path: &Path) -> Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn read_bounded(path: &Path, max_bytes: usize) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > max_bytes as u64
    {
        bail!("managed artifact file is not bounded and regular");
    }
    let bytes = read_no_follow(path)?;
    if bytes.len() > max_bytes {
        bail!("managed artifact file exceeds its bound");
    }
    Ok(bytes)
}

fn publish_noclobber(directory: &Path, destination: &Path, bytes: &[u8]) -> Result<()> {
    let staging = directory.join(format!(".{}.part", Uuid::new_v4().simple()));
    let mut file = create_private_file(&staging)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    match fs::hard_link(&staging, destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::read(destination)? != bytes {
                let _ = fs::remove_file(&staging);
                bail!("managed artifact manifest collision");
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&staging);
            return Err(error).context("failed to publish managed artifact manifest");
        }
    }
    let _ = fs::remove_file(staging);
    sync_parent(directory)
}

fn sync_parent(path: &Path) -> Result<()> {
    let directory = if path.is_dir() {
        path
    } else {
        path.parent().context("missing parent")?
    };
    #[cfg(unix)]
    {
        File::open(directory)?.sync_all()?;
        Ok(())
    }
    #[cfg(windows)]
    {
        // Windows does not provide stable directory-handle fsync semantics through Rust's
        // `File::sync_all`. Validate the parent without opening a directory handle; the
        // containing file was already flushed before publication and the namespace lock keeps
        // this operation serialized with other managed-artifact mutations.
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            bail!("managed artifact parent is not a real directory")
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = directory;
        bail!("managed artifact parent durability is unsupported on this platform")
    }
}

fn read_optional_file(path: &Path) -> Result<Vec<u8>> {
    match fs::symlink_metadata(path) {
        Ok(_) => read_no_follow(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn remove_capture_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn directory_file_bytes(root: &Path) -> Result<u64> {
    directory_file_usage(root).map(|(bytes, _)| bytes)
}

fn directory_file_usage(root: &Path) -> Result<(u64, u64)> {
    let mut pending = vec![root.to_path_buf()];
    let mut total = 0_u64;
    let mut count = 0_u64;
    while let Some(path) = pending.pop() {
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                bail!("managed artifact inventory contains a symlink");
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
                count = count.saturating_add(1);
            } else {
                bail!("managed artifact inventory contains a non-file entry");
            }
        }
    }
    Ok((total, count))
}

fn modified_at(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

#[cfg(test)]
#[path = "tests/managed_artifact_store_tests.rs"]
mod tests;
