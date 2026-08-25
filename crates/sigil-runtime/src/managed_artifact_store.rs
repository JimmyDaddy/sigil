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
    ToolArtifactProcessCaptureSnapshotV1, ToolArtifactRefV1, ToolArtifactSelectorV1,
    ToolArtifactStore, ToolArtifactStoreBackendV1, ToolArtifactTrashPruneReportV1,
    ToolOutputStreamV1,
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
}

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
        Self::publish_blob_locked(&staging, &store, content_sha256, bytes)
    }

    fn publish_descriptor_manifest(&self, descriptor: &ToolArtifactDescriptorV1) -> Result<()> {
        let (_operation, _staging, store) = self.with_mutation_lock()?;
        let refs = store.join("refs");
        create_private_dir(&refs)?;
        let path = refs.join(format!("{}.json", descriptor.artifact_ref.artifact_id));
        let bytes = serde_json::to_vec(descriptor)?;
        publish_noclobber(&refs, &path, &bytes)
    }

    fn read_blob(&self, content_sha256: &str) -> Result<Vec<u8>> {
        let (_operation, _staging, store) = self.with_mutation_lock()?;
        let path = blob_path(&store, content_sha256)?;
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > TOOL_ARTIFACT_MAX_BYTES as u64
        {
            bail!("managed artifact blob is not a bounded regular file");
        }
        let bytes = read_no_follow(&path)?;
        if hash_bytes(&bytes) != content_sha256 {
            bail!("managed artifact blob hash mismatch");
        }
        Ok(bytes)
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
        let descriptor = self.resolve(artifact_ref)?;
        let bytes = self.read_blob(&descriptor.content_sha256)?;
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

    fn prune_garbage_trash(
        &self,
        _now_unix_ms: u64,
        _trash_grace_ms: u64,
    ) -> Result<ToolArtifactTrashPruneReportV1> {
        bail!("managed artifact trash pruning requires the authority retire frontier")
    }

    fn begin_process_capture(
        self: Arc<Self>,
        _config: ProcessStreamCaptureConfigV1,
    ) -> Result<Box<dyn ToolArtifactProcessCaptureBackendV1>> {
        let staging = {
            let (_operation, staging, _store) = self.with_mutation_lock()?;
            staging
        };
        let directory = staging.join("staging");
        create_private_dir(&directory)?;
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
        self.stdout.take();
        self.stderr.take();
        let _ = fs::remove_file(&self.stdout_path);
        let _ = fs::remove_file(&self.stderr_path);
    }
}

impl ToolArtifactProcessCaptureBackendV1 for ManagedProcessCaptureBackend {
    fn write_stream(
        &mut self,
        stream: ToolOutputStreamV1,
        bytes: &[u8],
        limit: u64,
    ) -> Result<(u64, bool)> {
        let _ = self.owner.live_roots()?;
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
        if file.is_none() {
            *file = Some(create_private_file(path)?);
        }
        let before = *observed;
        *observed = observed.saturating_add(bytes.len() as u64);
        if before < limit {
            let allowed = (limit.saturating_sub(before) as usize).min(bytes.len());
            file.as_mut()
                .context("managed capture file is unavailable")?
                .write_all(&bytes[..allowed])?;
            if allowed < bytes.len() {
                *truncated = true;
            }
        } else {
            *truncated = true;
        }
        Ok((*observed, *truncated))
    }

    fn finish(mut self: Box<Self>) -> Result<ToolArtifactProcessCaptureSnapshotV1> {
        if let Some(file) = self.stdout.as_mut() {
            file.sync_all()?;
        }
        if let Some(file) = self.stderr.as_mut() {
            file.sync_all()?;
        }
        let stdout = read_optional_file(&self.stdout_path)?;
        let stderr = read_optional_file(&self.stderr_path)?;
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
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(directory)?;
        file.sync_all()?;
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

fn directory_file_bytes(root: &Path) -> Result<u64> {
    let mut pending = vec![root.to_path_buf()];
    let mut total = 0_u64;
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
            } else {
                bail!("managed artifact inventory contains a non-file entry");
            }
        }
    }
    Ok(total)
}

fn modified_at(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sigil_kernel::managed_storage::ManagedStorageServiceV1;
    use sigil_kernel::resource::{AuthorityGeneration, CanonicalHash};
    use sigil_resource_authority::storage::{
        AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
    };

    use super::*;

    fn writer(root: &Path) -> Arc<ManagedStorageWriterAdapterV1> {
        let mut table = AuthorityStorageGrantTableV1::new();
        table
            .register(crate::managed_storage_writer::grant_for_channel(
                StorageWriterChannelV1::ArtifactStaging,
                0x81,
            ))
            .expect("staging grant");
        table
            .register(crate::managed_storage_writer::grant_for_channel(
                StorageWriterChannelV1::ArtifactStore,
                0x82,
            ))
            .expect("store grant");
        let service: Arc<dyn ManagedStorageServiceV1> =
            Arc::new(AuthorityManagedStorageServiceV1::new(
                table,
                AuthorityGeneration {
                    epoch: 1,
                    instance_hash: CanonicalHash::from_bytes([0x28; 32]),
                },
            ));
        Arc::new(ManagedStorageWriterAdapterV1::new(
            service,
            root.to_path_buf(),
            CanonicalHash::from_bytes([0x84; 32]),
        ))
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
                stream_layout:
                    sigil_kernel::ToolOutputStreamLayoutV1::SeparatePipesNoCrossStreamOrder,
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
}
