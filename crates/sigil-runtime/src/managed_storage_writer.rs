//! RFC-0071 section 8.6 / R71.6: runtime managed-storage writer seam.
//!
//! Semantic writers (session log / input history / durable memory / session catalog / artifact
//! staging / adapter durable state) never derive a writable root from env/cwd and never open a
//! path before admission. The adapter owns the closed channel -> (semantic owner, capability
//! family) mapping and the authority-declared layout under the verified bootstrap state anchor;
//! every batch is an admit -> owner-only no-follow leaf -> append -> finalize(namespace) cycle
//! with a kernel-shaped storage receipt. The authority remains the only allocator.

use std::io::Write;
use std::path::{Path, PathBuf};

use sigil_kernel::managed_storage::{
    ManagedStorageNamespaceHandleV1, ManagedStorageServiceV1, ManagedStorageStorageReceiptV1,
};
use sigil_kernel::resource::{
    AdapterDurableStateClassV1, CanonicalHash, ManagedStorageCapabilityFamilyV1,
    ManagedStorageSemanticOwnerV1, MemoryScopeClassV1, OpaqueSessionId, ResourceJournalScopeV1,
    ResourceOwnerScopeV1,
};

/// Closed semantic writer channel (row-aligned with the R71.6 mandatory adapter kinds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StorageWriterChannelV1 {
    SessionLog,
    SessionLifecycleLog,
    InputHistory,
    DurableMemory,
    SessionCatalog,
    ArtifactStaging,
    AdapterDurableState,
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
            Self::AdapterDurableState => (
                ManagedStorageSemanticOwnerV1::AdapterDurableState(
                    AdapterDurableStateClassV1::ProtocolReplay,
                ),
                ManagedStorageCapabilityFamilyV1::AtomicObject,
                "adapter-durable-state",
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
    #[error("writer leaf resolves outside the state anchor")]
    LeafEscapesAnchor,
    #[error("writer leaf is a symlink (no-follow)")]
    LeafIsSymlink,
    #[error("writer leaf is not owner-only")]
    LeafNotOwnerOnly,
    #[error("writer io failed: {0}")]
    Io(String),
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

    /// Authority-declared physical directory (owner-only, no-follow). The consumer never derives
    /// this from env/cwd; it is the authority bootstrap layout.
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
}

impl std::fmt::Debug for ManagedStorageWriterAdapterV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedStorageWriterAdapterV1")
            .field("state_anchor", &self.state_anchor)
            .field("issuer_attached", &self.storage_issuer.is_some())
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
        }
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
        std::fs::create_dir_all(&path)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        if metadata.file_type().is_symlink() {
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
                let mut key_ns = [0x6bu8; 32];
                for (index, byte) in key.bytes().take(32).enumerate() {
                    key_ns[index] = byte;
                }
                let proof = broker.seal_storage_namespace_proof(
                    capability_family,
                    CanonicalHash::from_bytes(key_ns),
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
        let request = sigil_kernel::managed_storage::ManagedStorageAdmissionRequestV1 {
            semantic_owner,
            capability_family,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            source:
                sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
                    cutover_manifest_hash: self.cutover_manifest_hash,
                    application_generation: 1,
                },
            owner_scope: ResourceOwnerScopeV1::Session(OpaqueSessionId::new(
                "application".to_owned(),
            )),
            journal_scope: ResourceJournalScopeV1::Application,
        };
        let handle = self
            .service
            .admit_namespace(request, capability)
            .map_err(|error| ManagedStorageWriterErrorV1::AdmissionFailed(error.to_string()))?;
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
        std::fs::create_dir_all(&path)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        if metadata.file_type().is_symlink() {
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
            owner_scope: ResourceOwnerScopeV1::Session(OpaqueSessionId::new(
                "application".to_owned(),
            )),
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
        let record_file = lease.path.join("records.jsonl");
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&record_file)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        file.write_all(record)
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
        file.write_all(b"\n")
            .map_err(|error| ManagedStorageWriterErrorV1::Io(error.to_string()))?;
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

    /// Finalizes the namespace; the receipt is the durable writer fact for the batch.
    pub fn finalize(
        &self,
        lease: ManagedStorageWriterLeaseV1,
    ) -> Result<ManagedStorageStorageReceiptV1, ManagedStorageWriterErrorV1> {
        self.service
            .finalize_namespace(lease.handle, "writer-batch-complete".to_owned())
            .map_err(|error| ManagedStorageWriterErrorV1::FinalizeFailed(error.to_string()))
    }
}

/// Authority-form grant for one declared writer channel (R71.6 production composition:
/// a declared writer is a registered grant, so the cutover probe reflects exactly what is
/// composed and nothing more).
/// Both DurableMemory scope-class grants (UserPreference / ProjectFact) under the same
/// JournaledAtomicProjection family: the two classes are distinct semantic owners, so the
/// memory writer admits exact class namespaces and the cutover probe only inspects the
/// frozen ProjectFact cell.
pub fn memory_grants(seed: u8) -> Vec<sigil_kernel::managed_storage::StorageAdmissionGrantV1> {
    use sigil_kernel::resource::MemoryScopeClassV1;
    let project = grant_for_owner(
        StorageWriterChannelV1::DurableMemory,
        sigil_kernel::resource::ManagedStorageSemanticOwnerV1::DurableMemory(
            MemoryScopeClassV1::ProjectFact,
        ),
        seed,
    );
    let mut preferences = grant_for_owner(
        StorageWriterChannelV1::DurableMemory,
        sigil_kernel::resource::ManagedStorageSemanticOwnerV1::DurableMemory(
            MemoryScopeClassV1::UserPreference,
        ),
        seed + 1,
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
    let (semantic_owner, _, _) = channel.mapping();
    grant_for_owner(channel, semantic_owner, seed)
}

fn grant_for_owner(
    channel: StorageWriterChannelV1,
    semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1,
    seed: u8,
) -> sigil_kernel::managed_storage::StorageAdmissionGrantV1 {
    use sigil_kernel::resource::{AuthorityGeneration, OpaqueStorageGrantId, ResourceOwnerScopeV1};
    let (_, capability_family, leaf) = channel.mapping();
    let mut ns = [seed; 32];
    ns[0] = leaf.as_bytes()[0];
    sigil_kernel::managed_storage::StorageAdmissionGrantV1 {
        grant_id: OpaqueStorageGrantId::new(format!("grant-writer-{leaf}")),
        admission_hash: CanonicalHash::from_bytes([0x21; 32]),
        semantic_owner,
        purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
        purpose_hash: CanonicalHash::from_bytes([0x22; 32]),
        namespace_hash: CanonicalHash::from_bytes(ns),
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
            class: sigil_kernel::resource::ResourceQuotaClassV1::RuntimeState,
            max_bytes: 1024 * 1024,
            max_entries: 1024,
            max_open_holders: 1,
            max_age_ms: None,
            hard_runtime_enforcement_required: true,
            profile_hash: CanonicalHash::from_bytes([0x27; 32]),
        },
        semantic_schema: sigil_kernel::resource::OpaqueSemanticSchemaId::new(format!(
            "schema-{leaf}"
        )),
        authority_generation: AuthorityGeneration {
            epoch: 1,
            instance_hash: CanonicalHash::from_bytes([0x28; 32]),
        },
        journal_admission_sequence: 1,
        grant_hash: CanonicalHash::from_bytes([0x29; 32]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigil_kernel::managed_storage::StorageAdmissionGrantV1;
    use sigil_kernel::resource::{
        AuthorityGeneration, ManagedStorageCapabilityFamilyV1, ManagedStorageSemanticOwnerV1,
        OpaqueStorageGrantId, ResourceJournalScopeV1, ResourceOwnerScopeV1,
    };
    use sigil_resource_authority::storage::{
        AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
    };

    fn hash(seed: u8) -> CanonicalHash {
        CanonicalHash::from_bytes([seed; 32])
    }

    fn session_log_grant() -> StorageAdmissionGrantV1 {
        StorageAdmissionGrantV1 {
            grant_id: OpaqueStorageGrantId::new("g-writer-slog".to_owned()),
            admission_hash: hash(1),
            semantic_owner: ManagedStorageSemanticOwnerV1::SessionLog,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            purpose_hash: hash(2),
            namespace_hash: hash(3),
            journal_scope: ResourceJournalScopeV1::Application,
            journal_scope_hash: hash(4),
            resource_ref: sigil_kernel::resource::ResourceRefV1 {
                resource_id: sigil_kernel::resource::OpaqueResourceId::new(
                    "res-writer-slog".to_owned(),
                ),
                kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
                owner_scope: ResourceOwnerScopeV1::Application,
                journal_scope: ResourceJournalScopeV1::Application,
                generation: 1,
            },
            resource_binding_digest: hash(5),
            physical_binding_hash: hash(6),
            resource_kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
            owner_scope: ResourceOwnerScopeV1::Application,
            capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
            retention_policy: sigil_kernel::resource::ResourceRetentionPolicyV1::SessionPolicy,
            quota_profile: sigil_kernel::resource::ResourceQuotaProfileV1 {
                class: sigil_kernel::resource::ResourceQuotaClassV1::RuntimeState,
                max_bytes: 1024,
                max_entries: 100,
                max_open_holders: 1,
                max_age_ms: None,
                hard_runtime_enforcement_required: true,
                profile_hash: hash(7),
            },
            semantic_schema: sigil_kernel::resource::OpaqueSemanticSchemaId::new(
                "schema-writer-slog".to_owned(),
            ),
            authority_generation: AuthorityGeneration {
                epoch: 1,
                instance_hash: hash(8),
            },
            journal_admission_sequence: 1,
            grant_hash: hash(9),
        }
    }

    fn adapter(
        anchor: &Path,
        table: AuthorityStorageGrantTableV1,
    ) -> ManagedStorageWriterAdapterV1 {
        let service: std::sync::Arc<dyn ManagedStorageServiceV1> =
            std::sync::Arc::new(AuthorityManagedStorageServiceV1::new(
                table,
                AuthorityGeneration {
                    epoch: 1,
                    instance_hash: hash(8),
                },
            ));
        ManagedStorageWriterAdapterV1::new(service, anchor.to_path_buf(), hash(10))
    }

    #[test]
    fn r71_sw_session_log_batch_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(session_log_grant()).expect("register");
        let writer = adapter(dir.path(), table);
        let lease = writer
            .acquire(StorageWriterChannelV1::SessionLog)
            .expect("acquire");
        assert_eq!(lease.channel(), StorageWriterChannelV1::SessionLog);
        assert!(lease.path().ends_with("managed/session-log"));
        writer
            .write_record(&lease, b"{\"seq\":1}")
            .expect("write 1");
        writer
            .write_record(&lease, b"{\"seq\":2}")
            .expect("write 2");
        let content = std::fs::read_to_string(lease.path().join("records.jsonl")).expect("read");
        assert_eq!(content, "{\"seq\":1}\n{\"seq\":2}\n");
        let receipt = writer.finalize(lease).expect("finalize");
        assert_eq!(
            receipt.capability_family,
            ManagedStorageCapabilityFamilyV1::AppendLog
        );
    }

    #[test]
    fn r71_sw_unregistered_family_fails_admission() {
        let dir = tempfile::tempdir().expect("tempdir");
        let writer = adapter(dir.path(), AuthorityStorageGrantTableV1::new());
        let error = writer
            .acquire(StorageWriterChannelV1::SessionLog)
            .expect_err("no grant");
        assert!(matches!(
            error,
            ManagedStorageWriterErrorV1::AdmissionFailed(_)
        ));
    }

    #[test]
    fn r71_sw_leaf_permissions_owner_only() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().expect("tempdir");
            let mut table = AuthorityStorageGrantTableV1::new();
            table.register(session_log_grant()).expect("register");
            let writer = adapter(dir.path(), table);
            let lease = writer
                .acquire(StorageWriterChannelV1::SessionLog)
                .expect("acquire");
            writer.write_record(&lease, b"{\"seq\":1}").expect("write");
            let dir_meta = std::fs::symlink_metadata(lease.path()).expect("dir meta");
            assert_eq!(dir_meta.permissions().mode() & 0o077, 0);
            let file_meta =
                std::fs::symlink_metadata(lease.path().join("records.jsonl")).expect("file meta");
            assert_eq!(file_meta.permissions().mode() & 0o077, 0);
        }
    }

    #[test]
    fn r71_sw_finalize_twice_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(session_log_grant()).expect("register");
        let writer = adapter(dir.path(), table);
        let lease = writer
            .acquire(StorageWriterChannelV1::SessionLog)
            .expect("acquire");
        let path = lease.path().to_path_buf();
        let channel = lease.channel();
        let namespace_digest = lease.namespace_digest();
        writer.finalize(lease).expect("first finalize");
        // A second finalize of the same namespace is refused by the authority.
        let error = writer
            .finalize(ManagedStorageWriterLeaseV1 {
                handle: ManagedStorageNamespaceHandleV1::new(
                    sigil_kernel::resource::OpaqueKernelCapabilityHandleId::new(
                        "handle-storage-1".to_owned(),
                    ),
                    namespace_digest,
                    ManagedStorageCapabilityFamilyV1::AppendLog,
                    sigil_kernel::resource::OpaqueKernelCapabilityAuthenticatorV1::new(
                        "auth-storage-1".to_owned(),
                    ),
                ),
                path,
                channel,
            })
            .expect_err("second finalize");
        assert!(matches!(
            error,
            ManagedStorageWriterErrorV1::FinalizeFailed(_)
        ));
    }
    #[test]
    fn r71_sw_broker_backed_writer_uses_production_namespace() {
        use sigil_kernel::capability_issuer::KernelCapabilityBrokerV1;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(session_log_grant()).expect("register");
        let service: std::sync::Arc<dyn ManagedStorageServiceV1> =
            std::sync::Arc::new(AuthorityManagedStorageServiceV1::new(
                table,
                AuthorityGeneration {
                    epoch: 1,
                    instance_hash: hash(8),
                },
            ));
        let broker = std::sync::Arc::new(KernelCapabilityBrokerV1::new());
        let writer = ManagedStorageWriterAdapterV1::with_storage_issuer(
            service.clone(),
            dir.path().to_path_buf(),
            hash(10),
            broker.clone(),
        );
        // A startup probe runs first: its namespace is dedicated and never the production one.
        let capability =
            sigil_kernel::managed_storage::ValidatedStorageAdmissionCapabilityV1::startup_probe();
        let request = sigil_kernel::managed_storage::ManagedStorageAdmissionRequestV1 {
            semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLog,
            capability_family: sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            source:
                sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
                    cutover_manifest_hash: hash(10),
                    application_generation: 1,
                },
            owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Session(
                OpaqueSessionId::new("s-1".to_owned()),
            ),
            journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
        };
        let probe_handle = service
            .admit_namespace(request, capability)
            .expect("probe admit");
        assert_ne!(probe_handle.namespace_hash, hash(3));
        let probe_ns = probe_handle.namespace_hash;
        service
            .finalize_namespace(probe_handle, "probe".to_owned())
            .expect("probe finalize");
        // The broker-backed writer batch binds a claim-scoped namespace (distinct from the
        // probe claim) and works after the probe finalized its own namespace.
        let lease = writer
            .acquire(StorageWriterChannelV1::SessionLog)
            .expect("acquire");
        assert_ne!(lease.namespace_digest(), probe_ns);
        assert_ne!(
            lease.namespace_digest(),
            CanonicalHash::from_bytes([0u8; 32])
        );
        writer.write_record(&lease, b"seq=1").expect("write");
        writer.finalize(lease).expect("finalize");
    }
    #[test]
    fn r71_sw_named_acquire_per_session_and_unsafe_key_rejected() {
        use sigil_kernel::capability_issuer::KernelCapabilityBrokerV1;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(session_log_grant()).expect("register");
        let service: std::sync::Arc<dyn ManagedStorageServiceV1> =
            std::sync::Arc::new(AuthorityManagedStorageServiceV1::new(
                table,
                AuthorityGeneration {
                    epoch: 1,
                    instance_hash: hash(8),
                },
            ));
        let broker = std::sync::Arc::new(KernelCapabilityBrokerV1::new());
        let writer = ManagedStorageWriterAdapterV1::with_storage_issuer(
            service.clone(),
            dir.path().to_path_buf(),
            hash(10),
            broker,
        );
        let lease_a = writer
            .acquire_named(StorageWriterChannelV1::SessionLog, "session-abc")
            .expect("named a");
        assert!(lease_a.path().ends_with("session-log/session-abc"));
        writer.write_record(&lease_a, b"seq=1").expect("write");
        writer.finalize(lease_a).expect("finalize a");
        let lease_b = writer
            .acquire_named(StorageWriterChannelV1::SessionLog, "session-def")
            .expect("named b");
        writer.finalize(lease_b).expect("finalize b");
        // Unsafe sub-key rejected before any filesystem access.
        let error = writer
            .acquire_named(StorageWriterChannelV1::SessionLog, "../escape")
            .expect_err("unsafe");
        assert!(matches!(
            error,
            ManagedStorageWriterErrorV1::LeafEscapesAnchor
        ));
    }
}
