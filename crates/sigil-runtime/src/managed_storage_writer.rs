//! RFC-0071 section 8.6 / R71.6: runtime managed-storage writer seam.
//!
//! Semantic writers (session log / input history / durable memory / session catalog / artifact
//! staging / adapter durable state) never derive a writable root from env/cwd and never open a
//! path before admission. The adapter owns the closed channel -> (semantic owner, capability
//! family) mapping and the authority-declared layout under the verified bootstrap state anchor;
//! every batch is an admit -> owner-only no-follow leaf -> append -> finalize(namespace) cycle
//! with a kernel-shaped storage receipt. The authority remains the only allocator.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use sigil_kernel::managed_storage::{
    ManagedStorageNamespaceHandleV1, ManagedStorageServiceV1, ManagedStorageStorageReceiptV1,
};
use sigil_kernel::resource::{
    AdapterDurableStateClassV1, CanonicalHash, ManagedStorageCapabilityFamilyV1,
    ManagedStorageSemanticOwnerV1, MemoryScopeClassV1, ResourceJournalScopeV1,
    ResourceOwnerScopeV1,
};

/// Closed semantic writer channel (row-aligned with the R71.6 mandatory adapter kinds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StorageWriterChannelV1 {
    ApplicationControlLog,
    SessionLog,
    SessionLifecycleLog,
    InputHistory,
    DurableMemory,
    SessionCatalog,
    ArtifactStaging,
    ArtifactStore,
    AdapterDurableState,
    AdapterEgressDisclosure,
    AdapterIdempotencyLedger,
}

impl StorageWriterChannelV1 {
    /// Closed channel -> (semantic owner, capability family, leaf).
    pub const fn mapping(
        self,
    ) -> (
        ManagedStorageSemanticOwnerV1,
        ManagedStorageCapabilityFamilyV1,
        &'static str,
    ) {
        match self {
            Self::ApplicationControlLog => (
                ManagedStorageSemanticOwnerV1::ApplicationControlLog,
                ManagedStorageCapabilityFamilyV1::AppendLog,
                "application-control-log",
            ),
            Self::SessionLog => (
                ManagedStorageSemanticOwnerV1::SessionLog,
                ManagedStorageCapabilityFamilyV1::AppendLog,
                "session-log",
            ),
            Self::SessionLifecycleLog => (
                ManagedStorageSemanticOwnerV1::SessionLifecycleLog,
                ManagedStorageCapabilityFamilyV1::AppendLog,
                "session-lifecycle-log",
            ),
            Self::InputHistory => (
                ManagedStorageSemanticOwnerV1::InteractiveInputHistory,
                ManagedStorageCapabilityFamilyV1::AppendLog,
                "input-history",
            ),
            Self::DurableMemory => (
                ManagedStorageSemanticOwnerV1::DurableMemory(MemoryScopeClassV1::ProjectFact),
                ManagedStorageCapabilityFamilyV1::JournaledAtomicProjection,
                "durable-memory",
            ),
            Self::SessionCatalog => (
                ManagedStorageSemanticOwnerV1::SessionCatalog,
                // Frozen matrix cell: SessionCatalog is a rebuildable database projection.
                ManagedStorageCapabilityFamilyV1::RebuildableDatabaseProjection,
                "session-catalog",
            ),
            Self::ArtifactStaging => (
                ManagedStorageSemanticOwnerV1::ArtifactStaging,
                ManagedStorageCapabilityFamilyV1::StreamingArtifact,
                "artifact-staging",
            ),
            Self::ArtifactStore => (
                ManagedStorageSemanticOwnerV1::ArtifactStore,
                ManagedStorageCapabilityFamilyV1::ArtifactStore,
                "artifact-store",
            ),
            Self::AdapterDurableState => (
                ManagedStorageSemanticOwnerV1::AdapterDurableState(
                    AdapterDurableStateClassV1::ProtocolReplay,
                ),
                ManagedStorageCapabilityFamilyV1::AppendLog,
                "adapter-protocol-replay",
            ),
            Self::AdapterEgressDisclosure => (
                ManagedStorageSemanticOwnerV1::AdapterDurableState(
                    AdapterDurableStateClassV1::EgressDisclosure,
                ),
                ManagedStorageCapabilityFamilyV1::AppendLog,
                "adapter-egress-disclosure",
            ),
            Self::AdapterIdempotencyLedger => (
                ManagedStorageSemanticOwnerV1::AdapterDurableState(
                    AdapterDurableStateClassV1::IdempotencyLedger,
                ),
                ManagedStorageCapabilityFamilyV1::JournaledAtomicProjection,
                "adapter-idempotency-ledger",
            ),
        }
    }
}

/// Closed writer error taxonomy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManagedStorageWriterErrorV1 {
    #[error("managed storage admission failed: {0}")]
    AdmissionFailed(String),
    #[error("managed storage finalize failed: {0}")]
    FinalizeFailed(String),
    #[error("managed storage lease rejected: {0}")]
    LeaseRejected(String),
    #[error("writer leaf resolves outside the state anchor")]
    LeafEscapesAnchor,
    #[error("writer leaf is a symlink (no-follow)")]
    LeafIsSymlink,
    #[error("writer leaf is not owner-only")]
    LeafNotOwnerOnly,
    #[error("writer io failed: {0}")]
    Io(String),
    #[error("artifact retire authority is unavailable")]
    RetireAuthorityUnavailable,
    #[error("artifact retire authorization failed: {0}")]
    RetireAuthorizationFailed(String),
}

/// One admitted namespace lease: the authority-declared physical directory plus the handle.
#[derive(Debug)]
pub struct ManagedStorageWriterLeaseV1 {
    handle: ManagedStorageNamespaceHandleV1,
    path: PathBuf,
    pub(crate) channel: StorageWriterChannelV1,
}

impl ManagedStorageWriterLeaseV1 {
    /// Admitted namespace digest (the authority-one-shot identity for this lease).
    pub fn namespace_digest(&self) -> CanonicalHash {
        self.handle.namespace_hash
    }

    /// Authority-declared physical directory (owner-only, no-follow). Production mutations still
    /// go through this adapter's closed methods, which revalidate the admitted handle under the
    /// namespace lock before opening a managed object.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Closed writer channel this lease was admitted for.
    pub fn channel(&self) -> StorageWriterChannelV1 {
        self.channel
    }
}

/// Runtime managed-storage writer adapter (authority-owned layout under the state anchor).
pub struct ManagedStorageWriterAdapterV1 {
    service: std::sync::Arc<dyn ManagedStorageServiceV1>,
    state_anchor: PathBuf,
    cutover_manifest_hash: CanonicalHash,
    /// Real kernel capability broker (production): writer batches use broker-issued storage
    /// namespaces (production grant ns), never the kernel startup-probe marker.
    storage_issuer:
        Option<std::sync::Arc<sigil_kernel::capability_issuer::KernelCapabilityBrokerV1>>,
    artifact_retire_authority:
        Option<std::sync::Arc<sigil_resource_authority::maintenance::ArtifactRetireAuthorityV1>>,
}

impl std::fmt::Debug for ManagedStorageWriterAdapterV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedStorageWriterAdapterV1")
            .field("state_anchor", &self.state_anchor)
            .field("issuer_attached", &self.storage_issuer.is_some())
            .field(
                "artifact_retire_authority_attached",
                &self.artifact_retire_authority.is_some(),
            )
            .finish()
    }
}

impl ManagedStorageWriterAdapterV1 {
    /// Creates the adapter. `state_anchor` must be the authority-verified bootstrap state anchor
    /// (owner-only, no-follow); the adapter validates it before any acquire.
    pub fn new(
        service: std::sync::Arc<dyn ManagedStorageServiceV1>,
        state_anchor: PathBuf,
        cutover_manifest_hash: CanonicalHash,
    ) -> Self {
        Self {
            service,
            state_anchor,
            cutover_manifest_hash,
            storage_issuer: None,
            artifact_retire_authority: None,
        }
    }

    /// Production constructor: every acquire is backed by a real broker-issued storage
    /// namespace capability (one-shot proof), so writer batches bind the production grant
    /// namespace while startup probes keep their dedicated probe namespaces.
    pub fn with_storage_issuer(
        service: std::sync::Arc<dyn ManagedStorageServiceV1>,
        state_anchor: PathBuf,
        cutover_manifest_hash: CanonicalHash,
        storage_issuer: std::sync::Arc<sigil_kernel::capability_issuer::KernelCapabilityBrokerV1>,
    ) -> Self {
        Self {
            service,
            state_anchor,
            cutover_manifest_hash,
            storage_issuer: Some(storage_issuer),
            artifact_retire_authority: None,
        }
    }

    /// Attaches the authority-owned paired ArtifactStaging/ArtifactStore retire frontier.
    #[must_use]
    pub fn with_artifact_retire_authority(
        mut self,
        authority: std::sync::Arc<sigil_resource_authority::maintenance::ArtifactRetireAuthorityV1>,
    ) -> Self {
        self.artifact_retire_authority = Some(authority);
        self
    }

    /// Exchanges a pathless semantic eligibility frontier for an authority one-shot token.
    pub fn authorize_artifact_retirement(
        &self,
        frontier: sigil_kernel::session::ToolArtifactRetireFrontierV1,
    ) -> Result<
        sigil_resource_authority::maintenance::ArtifactRetireTokenV1,
        ManagedStorageWriterErrorV1,
    > {
        let authority = self
            .artifact_retire_authority
            .as_ref()
            .ok_or(ManagedStorageWriterErrorV1::RetireAuthorityUnavailable)?;
        authority
            .authorize(
                sigil_resource_authority::maintenance::ArtifactRetireEligibilityEvidenceV1 {
                    authority_generation: authority.authority_generation(),
                    artifact_staging_grant_hash: authority.artifact_staging_grant_hash(),
                    artifact_store_grant_hash: authority.artifact_store_grant_hash(),
                    selected_refs_hash: frontier.selected_refs_hash,
                    selected_count: frontier.selected_count,
                    selected_bytes: frontier.selected_bytes,
                    eligibility_frontier: frontier.eligibility_frontier,
                    policy_hash: frontier.policy_hash,
                },
            )
            .map_err(|error| {
                ManagedStorageWriterErrorV1::RetireAuthorizationFailed(error.to_string())
            })
    }

    /// Consumes the authority-issued artifact retirement proof at the physical writer boundary.
    pub fn consume_artifact_retirement(
        &self,
        token: &mut sigil_resource_authority::maintenance::ArtifactRetireTokenV1,
    ) -> Result<(), ManagedStorageWriterErrorV1> {
        token.consume_claim().map_err(|error| {
            ManagedStorageWriterErrorV1::RetireAuthorizationFailed(error.to_string())
        })
    }

    /// Authority-declared managed NAMED leaf path (validating, non-creating) so consumers can
    /// route per-session reads to the same leaf the writer uses.
    pub fn managed_named_leaf_path(
        &self,
        channel: StorageWriterChannelV1,
        key: &str,
    ) -> Result<PathBuf, ManagedStorageWriterErrorV1> {
        if key.is_empty()
            || key.len() > 64
            || !key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            return Err(ManagedStorageWriterErrorV1::LeafEscapesAnchor);
        }
        let (_, _, leaf) = channel.mapping();
        Ok(self.leaf_path(leaf)?.join(key))
    }

    /// Authority-declared managed leaf path for a channel without creating anything: read and
    /// write paths never diverge, so consumers route stored reads through the same leaf.
    pub fn managed_leaf_path(
        &self,
        channel: StorageWriterChannelV1,
    ) -> Result<PathBuf, ManagedStorageWriterErrorV1> {
        let (_, _, leaf) = channel.mapping();
        self.leaf_path(leaf)
    }

    fn leaf_path(&self, leaf: &str) -> Result<PathBuf, ManagedStorageWriterErrorV1> {
        // Canonicalize the verified anchor once (macOS /var -> /private/var); the leaf is
        // derived from the canonical anchor so the containment check is exact and the writer
        // never uses a non-canonical prefix.
        let prefix = self
            .state_anchor
            .canonicalize()
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        let resolved = prefix.join("managed").join(leaf);
        let resolved_abs = std::path::absolute(&resolved)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        if !resolved_abs.starts_with(&prefix) {
            return Err(ManagedStorageWriterErrorV1::LeafEscapesAnchor);
        }
        Ok(resolved)
    }

    /// Admit + prepare one NAMED namespace (per-session/per-object sub-key). The key must be
    /// opaque-bounded and safe for an authority-declared sub-leaf (no separators, no dots
    /// beyond one, length capped); illegal keys are rejected before any filesystem access.
    pub fn acquire_named(
        &self,
        channel: StorageWriterChannelV1,
        key: &str,
    ) -> Result<ManagedStorageWriterLeaseV1, ManagedStorageWriterErrorV1> {
        let (semantic_owner, _, _) = channel.mapping();
        self.acquire_owned(channel, semantic_owner, key)
    }

    /// Admit + prepare one NAMED durable-memory namespace for the exact scope class. The two
    /// DurableMemory scope classes are separate semantic owners (UserPreference / ProjectFact),
    /// so the channel mapping alone is not owner-exact; the caller names the class and the
    /// admission grant table has both grants registered by the composition.
    pub fn acquire_memory_namespace(
        &self,
        class: sigil_kernel::resource::MemoryScopeClassV1,
        key: &str,
    ) -> Result<ManagedStorageWriterLeaseV1, ManagedStorageWriterErrorV1> {
        use sigil_kernel::resource::ManagedStorageSemanticOwnerV1;
        self.acquire_owned(
            StorageWriterChannelV1::DurableMemory,
            ManagedStorageSemanticOwnerV1::DurableMemory(class),
            key,
        )
    }

    fn acquire_owned(
        &self,
        channel: StorageWriterChannelV1,
        semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1,
        key: &str,
    ) -> Result<ManagedStorageWriterLeaseV1, ManagedStorageWriterErrorV1> {
        if key.is_empty()
            || key.len() > 64
            || !key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            return Err(ManagedStorageWriterErrorV1::LeafEscapesAnchor);
        }
        let (_, capability_family, leaf) = channel.mapping();
        let path = self.leaf_path(leaf)?.join(key);
        if let Some(parent) = path.parent() {
            reject_existing_reparse_components(parent)?;
        }
        std::fs::create_dir_all(&path)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        reject_reparse_components(&path, false)?;
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        if !is_safe_physical_metadata(&metadata) || !metadata.is_dir() {
            return Err(ManagedStorageWriterErrorV1::LeafIsSymlink);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
            let flushed = std::fs::symlink_metadata(&path)
                .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
            if flushed.permissions().mode() & 0o077 != 0 {
                return Err(ManagedStorageWriterErrorV1::LeafNotOwnerOnly);
            }
        }
        let capability = match &self.storage_issuer {
            Some(broker) => {
                // The grant binds the authority-declared channel root. The logical key is
                // carried by the admitted physical sub-leaf, while each broker claim still
                // receives a distinct one-shot handle namespace in the authority.
                let proof = broker
                    .seal_storage_namespace_proof(capability_family, writer_namespace_hash(leaf));
                broker
                    .issue_storage_namespace_capability(proof)
                    .map_err(|error| {
                        ManagedStorageWriterErrorV1::AdmissionFailed(format!("{error:?}"))
                    })?
            }
            None => {
                sigil_kernel::managed_storage::ValidatedStorageAdmissionCapabilityV1::startup_probe(
                )
            }
        };
        let request = sigil_kernel::managed_storage::ManagedStorageAdmissionRequestV1 {
            semantic_owner,
            capability_family,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            source:
                sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
                    cutover_manifest_hash: self.cutover_manifest_hash,
                    application_generation: 1,
                },
            owner_scope: ResourceOwnerScopeV1::Application,
            journal_scope: ResourceJournalScopeV1::Application,
        };
        let handle = self
            .service
            .admit_namespace(request, capability)
            .map_err(|error| ManagedStorageWriterErrorV1::AdmissionFailed(error.to_string()))?;
        self.write_admission_marker(&path, &handle)?;
        Ok(ManagedStorageWriterLeaseV1 {
            handle,
            path,
            channel,
        })
    }

    /// Admit + prepare: one namespace per batch, physical leaf owner-only (0700) and no-follow.
    pub fn acquire(
        &self,
        channel: StorageWriterChannelV1,
    ) -> Result<ManagedStorageWriterLeaseV1, ManagedStorageWriterErrorV1> {
        let (semantic_owner, capability_family, leaf) = channel.mapping();
        let path = self.leaf_path(leaf)?;
        if let Some(parent) = path.parent() {
            reject_existing_reparse_components(parent)?;
        }
        std::fs::create_dir_all(&path)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        reject_reparse_components(&path, false)?;
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        if !is_safe_physical_metadata(&metadata) || !metadata.is_dir() {
            return Err(ManagedStorageWriterErrorV1::LeafIsSymlink);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
            let flushed = std::fs::symlink_metadata(&path)
                .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
            if flushed.permissions().mode() & 0o077 != 0 {
                return Err(ManagedStorageWriterErrorV1::LeafNotOwnerOnly);
            }
        }
        let request = sigil_kernel::managed_storage::ManagedStorageAdmissionRequestV1 {
            semantic_owner,
            capability_family,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            source:
                sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
                    cutover_manifest_hash: self.cutover_manifest_hash,
                    application_generation: 1,
                },
            owner_scope: ResourceOwnerScopeV1::Application,
            journal_scope: ResourceJournalScopeV1::Application,
        };
        let capability = match &self.storage_issuer {
            Some(broker) => {
                let mut leaf_ns = [0x6au8; 32];
                for (index, byte) in leaf.bytes().take(16).enumerate() {
                    leaf_ns[index] = byte;
                }
                let proof = broker.seal_storage_namespace_proof(
                    capability_family,
                    CanonicalHash::from_bytes(leaf_ns),
                );
                broker
                    .issue_storage_namespace_capability(proof)
                    .map_err(|error| {
                        ManagedStorageWriterErrorV1::AdmissionFailed(format!("{error:?}"))
                    })?
            }
            None => {
                sigil_kernel::managed_storage::ValidatedStorageAdmissionCapabilityV1::startup_probe(
                )
            }
        };
        let handle = self
            .service
            .admit_namespace(request, capability)
            .map_err(|error| ManagedStorageWriterErrorV1::AdmissionFailed(error.to_string()))?;
        self.write_admission_marker(&path, &handle)?;
        Ok(ManagedStorageWriterLeaseV1 {
            handle,
            path,
            channel,
        })
    }

    /// Appends one record to the admitted namespace leaf (0600 JSONL) and returns it; the
    /// namespace stays open for more records of the same batch.
    pub fn write_record(
        &self,
        lease: &ManagedStorageWriterLeaseV1,
        record: &[u8],
    ) -> Result<(), ManagedStorageWriterErrorV1> {
        let _namespace_lock = open_namespace_lock(&lease.path)?;
        self.service
            .validate_namespace_write(&lease.handle)
            .map_err(|error| ManagedStorageWriterErrorV1::LeaseRejected(error.to_string()))?;
        let record_file = lease.path.join("records.jsonl");
        reject_reparse_components(&record_file, true)?;
        let existed = std::fs::symlink_metadata(&record_file).is_ok();
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let mut file = options
            .open(&record_file)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        file.write_all(record)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        file.write_all(b"\n")
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        file.sync_all()
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        if !existed {
            sync_parent_directory(&record_file)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::symlink_metadata(&record_file)
                .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
            if metadata.permissions().mode() & 0o077 != 0 {
                std::fs::set_permissions(&record_file, std::fs::Permissions::from_mode(0o600))
                    .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
            }
        }
        Ok(())
    }

    fn write_admission_marker(
        &self,
        path: &Path,
        handle: &ManagedStorageNamespaceHandleV1,
    ) -> Result<(), ManagedStorageWriterErrorV1> {
        let marker = if let Some(admission) = handle.durable_admission() {
            serde_json::json!({
                "schema_version": 2,
                "handle_id": handle.handle_id.as_str(),
                "namespace_hash": handle.namespace_hash,
                "grant_hash": admission.grant_hash,
                "admission_sequence": admission.admission_sequence,
                "admission_record_hash": admission.admission_record_hash,
            })
        } else {
            serde_json::json!({
                "schema_version": 1,
                "handle_id": handle.handle_id.as_str(),
                "namespace_hash": handle.namespace_hash,
            })
        };
        let bytes = serde_json::to_vec(&marker)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        sigil_kernel::atomic_publish_private_file(&path.join("authority-admission.json"), &bytes)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))
    }

    fn physical_frontier(
        &self,
        lease: &ManagedStorageWriterLeaseV1,
    ) -> Result<(u64, u64, CanonicalHash), ManagedStorageWriterErrorV1> {
        let _namespace_lock = open_namespace_lock(&lease.path)?;
        let record_file = lease.path.join("records.jsonl");
        reject_reparse_components(&record_file, true)?;
        let bytes = match std::fs::symlink_metadata(&record_file) {
            Ok(metadata) => {
                if !is_safe_physical_metadata(&metadata) || !metadata.is_file() {
                    return Err(ManagedStorageWriterErrorV1::Io(
                        "managed record object must be a regular file".to_owned(),
                    ));
                }
                let mut options = std::fs::OpenOptions::new();
                options.read(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.custom_flags(libc::O_NOFOLLOW);
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::OpenOptionsExt;
                    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
                    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
                }
                let mut file = options
                    .open(&record_file)
                    .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
                file.sync_all()
                    .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)
                    .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
                bytes
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(ManagedStorageWriterErrorV1::Io(error.to_string())),
        };
        let record_count = managed_physical_record_count(lease.channel, &bytes)?;
        Ok((bytes.len() as u64, record_count, content_hash(&bytes)))
    }

    /// Reads the owner-controlled record object for an admitted namespace without following
    /// symlinks or exposing the physical root to the semantic adapter.
    pub fn read_record_bytes(
        &self,
        lease: &ManagedStorageWriterLeaseV1,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ManagedStorageWriterErrorV1> {
        let record_file = lease.path.join("records.jsonl");
        reject_reparse_components(&record_file, true)?;
        let metadata = match std::fs::symlink_metadata(&record_file) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(ManagedStorageWriterErrorV1::Io(error.to_string()));
            }
        };
        if !is_safe_physical_metadata(&metadata) || !metadata.is_file() {
            return Err(ManagedStorageWriterErrorV1::Io(
                "managed record object must be a regular file".to_owned(),
            ));
        }
        if metadata.len() > max_bytes as u64 {
            return Err(ManagedStorageWriterErrorV1::Io(format!(
                "managed record object exceeds {max_bytes} bytes"
            )));
        }
        let bytes = read_no_follow_file(&record_file)?;
        if bytes.len() > max_bytes {
            return Err(ManagedStorageWriterErrorV1::Io(format!(
                "managed record object exceeds {max_bytes} bytes"
            )));
        }
        Ok(bytes)
    }

    /// Replaces the owner-controlled record object atomically. This is used only by semantic
    /// adapters whose durable state is rebuilt from one bounded canonical snapshot; the caller
    /// still holds the admitted namespace for the whole replacement.
    pub fn replace_record_bytes(
        &self,
        lease: &ManagedStorageWriterLeaseV1,
        bytes: &[u8],
    ) -> Result<(), ManagedStorageWriterErrorV1> {
        let _namespace_lock = open_namespace_lock(&lease.path)?;
        self.service
            .validate_namespace_write(&lease.handle)
            .map_err(|error| ManagedStorageWriterErrorV1::LeaseRejected(error.to_string()))?;
        let record_file = lease.path.join("records.jsonl");
        reject_reparse_components(&record_file, true)?;
        if let Ok(metadata) = std::fs::symlink_metadata(&record_file)
            && (!is_safe_physical_metadata(&metadata) || !metadata.is_file())
        {
            return Err(ManagedStorageWriterErrorV1::Io(
                "managed record object must be a regular file".to_owned(),
            ));
        }
        sigil_kernel::atomic_publish_private_file(&record_file, bytes)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))
    }

    /// Finalizes the namespace; the receipt is the durable writer fact for the batch.
    pub fn finalize(
        &self,
        lease: ManagedStorageWriterLeaseV1,
    ) -> Result<ManagedStorageStorageReceiptV1, ManagedStorageWriterErrorV1> {
        let (byte_length, record_count, content_hash) = self.physical_frontier(&lease)?;
        self.service
            .finalize_namespace_with_physical_frontier(
                lease.handle,
                byte_length,
                record_count,
                content_hash,
                "writer-batch-complete".to_owned(),
            )
            .map_err(|error| ManagedStorageWriterErrorV1::FinalizeFailed(error.to_string()))
    }

    /// Internal artifact-layer liveness check. Artifact physical operations must keep the
    /// authority handle as the mutation capability; a copied root is not sufficient after the
    /// namespace has settled.
    pub(crate) fn validate_artifact_lease(
        &self,
        lease: &ManagedStorageWriterLeaseV1,
    ) -> Result<(), ManagedStorageWriterErrorV1> {
        self.service
            .validate_namespace_write(&lease.handle)
            .map_err(|error| ManagedStorageWriterErrorV1::LeaseRejected(error.to_string()))
    }

    pub(crate) fn reconcile_artifact_quota(
        &self,
        staging: &ManagedStorageWriterLeaseV1,
        staging_bytes: u64,
        staging_entries: u64,
        store: &ManagedStorageWriterLeaseV1,
        store_bytes: u64,
        store_entries: u64,
    ) -> Result<(), ManagedStorageWriterErrorV1> {
        self.service
            .reconcile_namespace_quota(&staging.handle, staging_bytes, staging_entries)
            .and_then(|_| {
                self.service
                    .reconcile_namespace_quota(&store.handle, store_bytes, store_entries)
            })
            .map_err(|error| ManagedStorageWriterErrorV1::LeaseRejected(error.to_string()))
    }
}

fn managed_physical_record_count(
    channel: StorageWriterChannelV1,
    bytes: &[u8],
) -> Result<u64, ManagedStorageWriterErrorV1> {
    if matches!(
        channel,
        StorageWriterChannelV1::AdapterDurableState
            | StorageWriterChannelV1::AdapterEgressDisclosure
            | StorageWriterChannelV1::AdapterIdempotencyLedger
    ) {
        if bytes.is_empty() {
            return Ok(0);
        }
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| {
            ManagedStorageWriterErrorV1::Io(
                "managed adapter snapshot is not a complete JSON object".to_owned(),
            )
        })?;
        return value.is_object().then_some(1).ok_or_else(|| {
            ManagedStorageWriterErrorV1::Io(
                "managed adapter snapshot must be a JSON object".to_owned(),
            )
        });
    }
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err(ManagedStorageWriterErrorV1::Io(
            "managed record object ends with a partial JSONL line".to_owned(),
        ));
    }
    Ok(bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .count() as u64)
}

fn content_hash(bytes: &[u8]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

fn open_namespace_lock(directory: &Path) -> Result<std::fs::File, ManagedStorageWriterErrorV1> {
    let path = directory.join(".authority-storage.lock");
    reject_reparse_components(&path, true)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE, WRITE_DAC,
            WRITE_OWNER,
        };
        options.access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE | WRITE_DAC | WRITE_OWNER);
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(&path)
        .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
    #[cfg(windows)]
    sigil_kernel::secure_private_path_permissions(&path)
        .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
    if !is_safe_physical_metadata(&metadata) || !metadata.is_file() {
        return Err(ManagedStorageWriterErrorV1::Io(
            "managed namespace lock must be a regular file".to_owned(),
        ));
    }
    file.lock_exclusive()
        .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
    Ok(file)
}

fn is_safe_physical_metadata(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    true
}

/// Rejects symlink/reparse ancestors before a physical managed object is opened. The final
/// component may be absent when the caller is about to create it; absent ancestors fail closed.
fn reject_reparse_components(
    path: &Path,
    allow_missing_leaf: bool,
) -> Result<(), ManagedStorageWriterErrorV1> {
    let components = path.components().collect::<Vec<_>>();
    let last = components.len().saturating_sub(1);
    let mut current = PathBuf::new();
    for (index, component) in components.into_iter().enumerate() {
        current.push(component.as_os_str());
        #[cfg(windows)]
        if matches!(component, std::path::Component::Prefix(_)) {
            // A drive or verbatim prefix (for example, `C:`) is not a filesystem entry. The
            // following root component produces the first inspectable path (`C:\`).
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && allow_missing_leaf
                    && index == last =>
            {
                continue;
            }
            Err(error) => return Err(ManagedStorageWriterErrorV1::Io(error.to_string())),
        };
        if !is_safe_physical_metadata(&metadata) {
            return Err(ManagedStorageWriterErrorV1::Io(format!(
                "physical managed path contains a symlink or reparse point: {}",
                current.display()
            )));
        }
    }
    Ok(())
}

/// Performs the same check for a path that may still have a missing directory suffix. This is
/// used immediately before `create_dir_all`; after creation the complete path is checked with
/// `reject_reparse_components` before any managed file is opened.
fn reject_existing_reparse_components(path: &Path) -> Result<(), ManagedStorageWriterErrorV1> {
    let components = path.components();
    let mut current = PathBuf::new();
    for component in components {
        current.push(component.as_os_str());
        #[cfg(windows)]
        if matches!(component, std::path::Component::Prefix(_)) {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(ManagedStorageWriterErrorV1::Io(error.to_string())),
        };
        if !is_safe_physical_metadata(&metadata) {
            return Err(ManagedStorageWriterErrorV1::Io(format!(
                "physical managed path contains a symlink or reparse point: {}",
                current.display()
            )));
        }
    }
    Ok(())
}

fn read_no_follow_file(path: &Path) -> Result<Vec<u8>, ManagedStorageWriterErrorV1> {
    reject_reparse_components(path, false)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = options
        .open(path)
        .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
    if !is_safe_physical_metadata(&metadata) || !metadata.is_file() {
        return Err(ManagedStorageWriterErrorV1::Io(
            "managed record object must be a regular non-reparse file".to_owned(),
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
    Ok(bytes)
}

fn sync_parent_directory(path: &Path) -> Result<(), ManagedStorageWriterErrorV1> {
    let parent = path.parent().ok_or_else(|| {
        ManagedStorageWriterErrorV1::Io("managed record path has no parent".to_owned())
    })?;
    #[cfg(unix)]
    {
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        let metadata = std::fs::symlink_metadata(parent)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(ManagedStorageWriterErrorV1::Io(
                "managed record parent is not a real Windows directory".to_owned(),
            ));
        }
        // Stable Windows does not support syncing a directory handle with `File::sync_all()`.
        // The record file was already synced above; its private atomic publication is
        // write-through on Windows, so the parent directory is only validated here.
    }
    #[cfg(not(any(unix, windows)))]
    {
        return Err(ManagedStorageWriterErrorV1::Io(
            "managed record parent durability is unsupported on this platform".to_owned(),
        ));
    }
    Ok(())
}

/// Authority-form grant for one declared writer channel (R71.6 production composition:
/// a declared writer is a registered grant, so the cutover probe reflects exactly what is
/// composed and nothing more).
/// Both DurableMemory scope-class grants (UserPreference / ProjectFact) under the same
/// JournaledAtomicProjection family: the two classes are distinct semantic owners, so the
/// memory writer admits exact class namespaces and the cutover probe only inspects the
/// frozen ProjectFact cell.
pub fn memory_grants(seed: u8) -> Vec<sigil_kernel::managed_storage::StorageAdmissionGrantV1> {
    memory_grants_with_context(
        seed,
        sigil_kernel::resource::AuthorityGeneration {
            epoch: 1,
            instance_hash: CanonicalHash::from_bytes([0x28; 32]),
        },
        CanonicalHash::from_bytes([0u8; 32]),
    )
}

pub fn memory_grants_with_context(
    seed: u8,
    authority_generation: sigil_kernel::resource::AuthorityGeneration,
    source_binding_hash: CanonicalHash,
) -> Vec<sigil_kernel::managed_storage::StorageAdmissionGrantV1> {
    use sigil_kernel::resource::MemoryScopeClassV1;
    let project = grant_for_owner(
        StorageWriterChannelV1::DurableMemory,
        sigil_kernel::resource::ManagedStorageSemanticOwnerV1::DurableMemory(
            MemoryScopeClassV1::ProjectFact,
        ),
        seed,
        authority_generation,
        source_binding_hash,
    );
    let mut preferences = grant_for_owner(
        StorageWriterChannelV1::DurableMemory,
        sigil_kernel::resource::ManagedStorageSemanticOwnerV1::DurableMemory(
            MemoryScopeClassV1::UserPreference,
        ),
        seed + 1,
        authority_generation,
        source_binding_hash,
    );
    // Both classes are grants of the same writer channel: grant ids must stay distinct for
    // the authority tables (a duplicate id would be a capability mismatch at registration).
    preferences.grant_id = sigil_kernel::resource::OpaqueStorageGrantId::new(
        "grant-writer-durable-memory-preferences".to_owned(),
    );
    vec![project, preferences]
}

pub fn grant_for_channel(
    channel: StorageWriterChannelV1,
    seed: u8,
) -> sigil_kernel::managed_storage::StorageAdmissionGrantV1 {
    grant_for_channel_with_context(
        channel,
        seed,
        sigil_kernel::resource::AuthorityGeneration {
            epoch: 1,
            instance_hash: CanonicalHash::from_bytes([0x28; 32]),
        },
        CanonicalHash::from_bytes([0u8; 32]),
    )
}

pub fn grant_for_channel_with_context(
    channel: StorageWriterChannelV1,
    seed: u8,
    authority_generation: sigil_kernel::resource::AuthorityGeneration,
    source_binding_hash: CanonicalHash,
) -> sigil_kernel::managed_storage::StorageAdmissionGrantV1 {
    let (semantic_owner, _, _) = channel.mapping();
    grant_for_owner(
        channel,
        semantic_owner,
        seed,
        authority_generation,
        source_binding_hash,
    )
}

fn grant_for_owner(
    channel: StorageWriterChannelV1,
    semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1,
    seed: u8,
    authority_generation: sigil_kernel::resource::AuthorityGeneration,
    source_binding_hash: CanonicalHash,
) -> sigil_kernel::managed_storage::StorageAdmissionGrantV1 {
    use sigil_kernel::resource::{OpaqueStorageGrantId, ResourceOwnerScopeV1};
    let (_, capability_family, leaf) = channel.mapping();
    let namespace_hash = writer_namespace_hash(leaf);
    let (quota_class, quota_max_bytes, quota_max_entries, quota_max_holders) = match channel {
        StorageWriterChannelV1::ArtifactStaging => (
            sigil_kernel::resource::ResourceQuotaClassV1::ArtifactStaging,
            sigil_kernel::session::TOOL_ARTIFACT_SESSION_BUDGET_BYTES,
            100_000,
            1_024,
        ),
        StorageWriterChannelV1::ArtifactStore => (
            sigil_kernel::resource::ResourceQuotaClassV1::ArtifactStore,
            sigil_kernel::session::TOOL_ARTIFACT_SESSION_BUDGET_BYTES,
            100_000,
            1_024,
        ),
        _ => (
            sigil_kernel::resource::ResourceQuotaClassV1::RuntimeState,
            1024 * 1024,
            1024,
            1,
        ),
    };
    sigil_kernel::managed_storage::StorageAdmissionGrantV1 {
        grant_id: OpaqueStorageGrantId::new(format!("grant-writer-{leaf}")),
        admission_hash: CanonicalHash::from_bytes([0x21 ^ seed; 32]),
        semantic_owner,
        purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
        purpose_hash: CanonicalHash::from_bytes([0x22; 32]),
        source_class: sigil_kernel::resource::StorageAdmissionSourceClassV1::ApplicationCutoverRoot,
        source_binding_hash,
        namespace_hash,
        journal_scope: ResourceJournalScopeV1::Application,
        journal_scope_hash: CanonicalHash::from_bytes([0x24; 32]),
        resource_ref: sigil_kernel::resource::ResourceRefV1 {
            resource_id: sigil_kernel::resource::OpaqueResourceId::new(format!("res-{leaf}")),
            kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
            owner_scope: ResourceOwnerScopeV1::Application,
            journal_scope: ResourceJournalScopeV1::Application,
            generation: 1,
        },
        resource_binding_digest: CanonicalHash::from_bytes([0x25; 32]),
        physical_binding_hash: CanonicalHash::from_bytes([0x26; 32]),
        resource_kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
        owner_scope: ResourceOwnerScopeV1::Application,
        capability_family,
        retention_policy: sigil_kernel::resource::ResourceRetentionPolicyV1::SessionPolicy,
        quota_profile: sigil_kernel::resource::ResourceQuotaProfileV1 {
            class: quota_class,
            max_bytes: quota_max_bytes,
            max_entries: quota_max_entries,
            max_open_holders: quota_max_holders,
            max_age_ms: None,
            hard_runtime_enforcement_required: true,
            profile_hash: CanonicalHash::from_bytes([0x27; 32]),
        },
        semantic_schema: sigil_kernel::resource::OpaqueSemanticSchemaId::new(format!(
            "schema-{leaf}"
        )),
        authority_generation,
        journal_admission_sequence: 1,
        grant_hash: hash_grant_identity(leaf, authority_generation, source_binding_hash),
    }
}

fn writer_namespace_hash(leaf: &str) -> CanonicalHash {
    let mut namespace = [0x6au8; 32];
    for (index, byte) in leaf.bytes().take(16).enumerate() {
        namespace[index] = byte;
    }
    CanonicalHash::from_bytes(namespace)
}

fn hash_grant_identity(
    leaf: &str,
    authority_generation: sigil_kernel::resource::AuthorityGeneration,
    source_binding_hash: CanonicalHash,
) -> CanonicalHash {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(leaf.as_bytes());
    bytes.extend_from_slice(&authority_generation.epoch.to_be_bytes());
    bytes.extend_from_slice(authority_generation.instance_hash.as_bytes());
    bytes.extend_from_slice(source_binding_hash.as_bytes());
    crate::r71_shadow_planner::canonical_digest(&bytes)
}

#[cfg(test)]
#[path = "tests/managed_storage_writer_tests.rs"]
mod tests;
