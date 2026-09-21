//! RFC-0071 section 8.6: host-owned managed storage port.
//!
//! Semantic writers acquire a pathless ManagedStorageNamespaceHandleV1 through the kernel-issued
//! validated capability; logical keys and artifact publish tokens are kernel-broker constructed
//! only. Local descriptors, locks, authority tokens and primitive leases stay in the authority
//! implementation and never cross this port.

use serde::{Deserialize, Serialize};

use crate::resource::{
    AuthorityGeneration, BoundedVec, CanonicalHash, ManagedStorageAdmissionPurposeV1,
    ManagedStorageCapabilityFamilyV1, ManagedStorageSemanticOwnerV1, OpaqueArtifactId,
    OpaqueBlobWriterId, OpaqueKernelCapabilityAuthenticatorV1, OpaqueKernelCapabilityHandleId,
    OpaquePublishTransactionId, OpaqueResourceId, OpaqueSemanticSchemaId, OpaqueStagedBlobRef,
    OpaqueStorageGrantId, OpaqueStorageKeyIdV1, ResourceAuthorityScopeV1, ResourceKindV1,
    ResourceOwnerScopeV1, ResourceQuotaProfileV1, ResourceRefV1, ResourceRetentionPolicyV1,
    StorageAdmissionSourceClassV1, StorageLogicalKeyKindV1,
};

pub const MAX_STORAGE_LOGICAL_KEY_ATOMS: usize = 8;

/// Evidence naming one already-published physical namespace.
///
/// This is only a stable business-key locator. It is never a capability and is revalidated
/// against the current grant, current authority generation and the protected namespace marker
/// before a new in-memory lease is issued after restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedStorageExistingNamespaceBindingV1 {
    pub original_handle_id: OpaqueKernelCapabilityHandleId,
    pub original_namespace_hash: CanonicalHash,
}

/// Opaque storage namespace handle (kernel-broker constructed; non-clone).
#[derive(Debug)]
pub struct ManagedStorageNamespaceHandleV1 {
    pub handle_id: OpaqueKernelCapabilityHandleId,
    pub namespace_hash: CanonicalHash,
    pub capability_family: ManagedStorageCapabilityFamilyV1,
    #[allow(dead_code)]
    authenticator: OpaqueKernelCapabilityAuthenticatorV1,
}

impl ManagedStorageNamespaceHandleV1 {
    pub const fn new(
        handle_id: OpaqueKernelCapabilityHandleId,
        namespace_hash: CanonicalHash,
        capability_family: ManagedStorageCapabilityFamilyV1,
        authenticator: OpaqueKernelCapabilityAuthenticatorV1,
    ) -> Self {
        Self {
            handle_id,
            namespace_hash,
            capability_family,
            authenticator,
        }
    }
}

/// Closed storage grant (durable namespace admission fact).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageAdmissionGrantV1 {
    pub grant_id: OpaqueStorageGrantId,
    pub admission_hash: CanonicalHash,
    pub semantic_owner: ManagedStorageSemanticOwnerV1,
    pub purpose: ManagedStorageAdmissionPurposeV1,
    pub purpose_hash: CanonicalHash,
    pub source_class: StorageAdmissionSourceClassV1,
    pub source_binding_hash: CanonicalHash,
    pub namespace_hash: CanonicalHash,
    pub authority_scope: ResourceAuthorityScopeV1,
    pub authority_scope_hash: CanonicalHash,
    pub resource_ref: ResourceRefV1,
    pub resource_binding_digest: CanonicalHash,
    pub physical_binding_hash: CanonicalHash,
    pub resource_kind: ResourceKindV1,
    pub owner_scope: ResourceOwnerScopeV1,
    pub capability_family: ManagedStorageCapabilityFamilyV1,
    pub retention_policy: ResourceRetentionPolicyV1,
    pub quota_profile: ResourceQuotaProfileV1,
    pub semantic_schema: OpaqueSemanticSchemaId,
    pub authority_generation: AuthorityGeneration,
    pub grant_hash: CanonicalHash,
}

/// Logical key atoms (closed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageLogicalKeyAtomV1 {
    StableLabel(String),
    StableId(String),
    Digest(CanonicalHash),
    Unsigned(u64),
}

/// Logical key descriptor (caller submits atoms; authority never interprets text as a path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageLogicalKeyDescriptorV1 {
    pub semantic_schema: OpaqueSemanticSchemaId,
    pub atoms: BoundedVec<StorageLogicalKeyAtomV1, MAX_STORAGE_LOGICAL_KEY_ATOMS>,
    pub descriptor_hash: CanonicalHash,
}

/// Opaque object key (kernel-broker constructed).
#[derive(Debug)]
pub struct OpaqueStorageObjectKeyV1 {
    pub key_id: OpaqueStorageKeyIdV1,
    pub namespace_hash: CanonicalHash,
    pub semantic_schema: OpaqueSemanticSchemaId,
    pub descriptor_hash: CanonicalHash,
    pub registration_record_hash: CanonicalHash,
    #[allow(dead_code)]
    authenticator: OpaqueKernelCapabilityAuthenticatorV1,
}

/// Opaque stream key (kernel-broker constructed).
#[derive(Debug)]
pub struct OpaqueStorageStreamKeyV1 {
    pub key_id: OpaqueStorageKeyIdV1,
    pub namespace_hash: CanonicalHash,
    pub semantic_schema: OpaqueSemanticSchemaId,
    pub descriptor_hash: CanonicalHash,
    pub registration_record_hash: CanonicalHash,
    #[allow(dead_code)]
    authenticator: OpaqueKernelCapabilityAuthenticatorV1,
}

/// Registered logical key payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageLogicalKeyRegisteredPayloadV1 {
    pub key_id: OpaqueStorageKeyIdV1,
    pub grant_id: OpaqueStorageGrantId,
    pub grant_hash: CanonicalHash,
    pub namespace_hash: CanonicalHash,
    pub semantic_schema: OpaqueSemanticSchemaId,
    pub key_kind: StorageLogicalKeyKindV1,
    pub descriptor_hash: CanonicalHash,
    pub encoded_safe_component_hash: CanonicalHash,
    pub authority_generation: AuthorityGeneration,
    pub payload_hash: CanonicalHash,
}

/// Artifact publish admission (dual-grant staging + store).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactPublishAdmissionV1 {
    pub transaction_id: OpaquePublishTransactionId,
    pub writer_id: OpaqueBlobWriterId,
    pub staged_blob_ref: OpaqueStagedBlobRef,
    pub writer_seal_hash: CanonicalHash,
    pub expected_content_digest: CanonicalHash,
    pub expected_byte_length: u64,
    pub artifact_object_key_hash: CanonicalHash,
    pub authority_generation: AuthorityGeneration,
    pub authority_scope: ResourceAuthorityScopeV1,
    pub staging_namespace_hash: CanonicalHash,
    pub store_namespace_hash: CanonicalHash,
}

/// Closed storage admission source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageAdmissionSourceV1 {
    ApplicationCutoverRoot {
        cutover_manifest_hash: CanonicalHash,
        application_generation: u64,
    },
    ApplicationControlReady {
        cutover_manifest_hash: CanonicalHash,
        application_generation: u64,
        control_grant_hash: CanonicalHash,
        control_admission_frontier_hash: CanonicalHash,
    },
    ApplicationLifecycleReady {
        cutover_manifest_hash: CanonicalHash,
        application_generation: u64,
        control_grant_hash: CanonicalHash,
        control_frontier_hash: CanonicalHash,
        lifecycle_grant_hash: CanonicalHash,
        lifecycle_admission_frontier_hash: CanonicalHash,
    },
    SessionLifecycle {
        session_scope: String,
        session_generation: u64,
        workspace_scope: String,
        lifecycle_event_digest: CanonicalHash,
        lifecycle_log_grant_hash: CanonicalHash,
        lifecycle_frontier_hash: CanonicalHash,
    },
    WorkspaceLifecycle {
        workspace_scope: String,
        workspace_generation: u64,
        lifecycle_event_digest: CanonicalHash,
        lifecycle_log_grant_hash: CanonicalHash,
        lifecycle_frontier_hash: CanonicalHash,
    },
    ToolDecisionExecution {
        permission_plan_hash: CanonicalHash,
        decision_hash: CanonicalHash,
        execution_draft_hash: CanonicalHash,
    },
    ToolDecisionInProcessStorage {
        storage_plan_hash: CanonicalHash,
        requirement_set_hash: CanonicalHash,
        operation_digest: CanonicalHash,
        decision_hash: CanonicalHash,
    },
    ExtensionDecision {
        extension_plan_hash: CanonicalHash,
        extension_decision_hash: CanonicalHash,
    },
    SemanticTransaction {
        transaction_id: String,
        transaction_hash: CanonicalHash,
    },
    RecoveryAction {
        action_token_hash: CanonicalHash,
        blocker_id: String,
    },
}

impl StorageAdmissionSourceV1 {
    pub const fn source_class(&self) -> StorageAdmissionSourceClassV1 {
        match self {
            Self::ApplicationCutoverRoot { .. } => {
                StorageAdmissionSourceClassV1::ApplicationCutoverRoot
            }
            Self::ApplicationControlReady { .. } => {
                StorageAdmissionSourceClassV1::ApplicationControlReady
            }
            Self::ApplicationLifecycleReady { .. } => {
                StorageAdmissionSourceClassV1::ApplicationLifecycleReady
            }
            Self::SessionLifecycle { .. } => StorageAdmissionSourceClassV1::SessionLifecycle,
            Self::WorkspaceLifecycle { .. } => StorageAdmissionSourceClassV1::WorkspaceLifecycle,
            Self::ToolDecisionExecution { .. } => {
                StorageAdmissionSourceClassV1::ToolDecisionExecution
            }
            Self::ToolDecisionInProcessStorage { .. } => {
                StorageAdmissionSourceClassV1::ToolDecisionInProcessStorage
            }
            Self::ExtensionDecision { .. } => StorageAdmissionSourceClassV1::ExtensionDecision,
            Self::SemanticTransaction { .. } => StorageAdmissionSourceClassV1::SemanticTransaction,
            Self::RecoveryAction { .. } => StorageAdmissionSourceClassV1::RecoveryAction,
        }
    }
}

/// Namespace admission request (pathless).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedStorageAdmissionRequestV1 {
    pub semantic_owner: ManagedStorageSemanticOwnerV1,
    pub capability_family: ManagedStorageCapabilityFamilyV1,
    pub purpose: ManagedStorageAdmissionPurposeV1,
    pub source: StorageAdmissionSourceV1,
    pub owner_scope: ResourceOwnerScopeV1,
    pub authority_scope: ResourceAuthorityScopeV1,
    /// Stable business identity for the physical namespace. The authority uses this value,
    /// never a process-local sequence, to locate the namespace after restart.
    pub namespace_key_hash: CanonicalHash,
}

/// Validated storage admission capability (kernel-issued; non-clone, non-serialize).
#[derive(Debug)]
pub struct ValidatedStorageAdmissionCapabilityV1 {
    pub handle_id: OpaqueKernelCapabilityHandleId,
    binding: Option<StorageAdmissionCapabilityBindingV1>,
    #[allow(dead_code)]
    authenticator: OpaqueKernelCapabilityAuthenticatorV1,
}

/// Kernel-sealed binding carried by a broker-issued storage capability. The value is observable
/// only through an already validated capability; callers cannot construct one or replace it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageAdmissionCapabilityBindingV1 {
    family: ManagedStorageCapabilityFamilyV1,
    namespace_hash: CanonicalHash,
}

impl StorageAdmissionCapabilityBindingV1 {
    #[must_use]
    pub const fn family(self) -> ManagedStorageCapabilityFamilyV1 {
        self.family
    }

    #[must_use]
    pub const fn namespace_hash(self) -> CanonicalHash {
        self.namespace_hash
    }
}

impl ValidatedStorageAdmissionCapabilityV1 {
    /// Kernel-broker-issued admission capability (real; distinct from the probe marker).
    pub(crate) fn broker_issued(
        handle_id: OpaqueKernelCapabilityHandleId,
        family: ManagedStorageCapabilityFamilyV1,
        namespace_hash: CanonicalHash,
    ) -> Self {
        Self {
            authenticator: OpaqueKernelCapabilityAuthenticatorV1::new(format!(
                "auth-{}",
                handle_id.as_str()
            )),
            handle_id,
            binding: Some(StorageAdmissionCapabilityBindingV1 {
                family,
                namespace_hash,
            }),
        }
    }

    /// Kernel-owned startup readiness probe handle (R71.6). This is NOT a real admission;
    /// services must treat it as probe-only and real admissions must be issuer-issued. It
    /// exists so the mandatory adapter readiness check can run a round trip without a
    /// consumer fabricating a handle.
    pub fn startup_probe() -> Self {
        Self {
            handle_id: OpaqueKernelCapabilityHandleId::new("startup-probe".to_owned()),
            authenticator: OpaqueKernelCapabilityAuthenticatorV1::new("startup-probe".to_owned()),
            binding: None,
        }
    }

    #[must_use]
    pub const fn binding(&self) -> Option<StorageAdmissionCapabilityBindingV1> {
        self.binding
    }
}

/// Storage outcome envelope: semantic result plus managed-storage receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedStorageResultV1 {
    pub storage_receipt: ManagedStorageStorageReceiptV1,
    pub result_digest: CanonicalHash,
}

/// Reduced storage receipt. The committed version is local to the current namespace and is not
/// a replay cursor or authority state sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedStorageStorageReceiptV1 {
    pub grant_id: OpaqueStorageGrantId,
    pub grant_hash: CanonicalHash,
    pub semantic_owner: ManagedStorageSemanticOwnerV1,
    pub capability_family: ManagedStorageCapabilityFamilyV1,
    pub resource_id: OpaqueResourceId,
    pub operation_digest: CanonicalHash,
    pub committed_entry_count: Option<u64>,
    pub committed_frontier_hash: CanonicalHash,
    pub receipt_hash: CanonicalHash,
    /// Authority-owned observation of the bytes committed by this operation.
    #[serde(default)]
    pub physical_frontier_hash: Option<CanonicalHash>,
}

/// Stable user-reviewed request for one control-log generation recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlLogRecoveryRequestV1 {
    pub logical_journal_id: CanonicalHash,
    pub operation_id: String,
    pub from_generation: u64,
    pub successor_generation: u64,
    pub header_digest: CanonicalHash,
    /// Opaque semantic-owner context; RA binds its digest without interpreting command state.
    pub owner_context_digest: CanonicalHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlLogRecoveryPreviewV1 {
    pub request: ControlLogRecoveryRequestV1,
    pub old_namespace_hash: CanonicalHash,
    pub successor_namespace_hash: CanonicalHash,
    pub old_byte_length: u64,
    pub old_content_digest: CanonicalHash,
    pub old_file_identity: Option<CanonicalHash>,
    pub preview_digest: CanonicalHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlLogRecoveryPhaseV1 {
    Prepared,
    Sealed,
    HeaderInitialized,
    Activated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlLogRecoveryStateV1 {
    pub preview: ControlLogRecoveryPreviewV1,
    pub phase: ControlLogRecoveryPhaseV1,
    pub previous_phase_digest: CanonicalHash,
    pub phase_digest: CanonicalHash,
}

/// Pathless guard retaining the physical namespace lock until the dispatch boundary returns.
pub trait ManagedStorageForwardGuardV1: Send {}

/// Consumer facing pathless managed storage service (authority implementation).
pub trait ManagedStorageServiceV1: Send + Sync {
    fn admit_namespace(
        &self,
        request: ManagedStorageAdmissionRequestV1,
        capability: ValidatedStorageAdmissionCapabilityV1,
    ) -> Result<ManagedStorageNamespaceHandleV1, ManagedStorageErrorV1>;

    /// Consumes a fresh storage capability to attach to an already materialized namespace. The
    /// supplied binding is evidence only; implementations must revalidate the current grant,
    /// authority generation, protected marker and stable namespace identity before issuing a
    /// new in-memory lease.
    fn admit_existing_namespace(
        &self,
        _request: ManagedStorageAdmissionRequestV1,
        _capability: ValidatedStorageAdmissionCapabilityV1,
        _original: ManagedStorageExistingNamespaceBindingV1,
    ) -> Result<ManagedStorageNamespaceHandleV1, ManagedStorageErrorV1> {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    }

    /// Validates that a physical mutation is still covered by an admitted namespace. The
    /// runtime calls this while holding the namespace lock, so settlement wins the race before a
    /// post-settlement write can reach the physical object. Existing reserved capacity remains
    /// usable only while the shared quota authority's durability state is healthy.
    ///
    /// # Errors
    /// Rejects a stale or mismatched handle, unavailable authority, or uncertain quota
    /// durability. This validation must not perform a new reservation or journal mutation.
    fn validate_namespace_write(
        &self,
        handle: &ManagedStorageNamespaceHandleV1,
    ) -> Result<(), ManagedStorageErrorV1>;

    fn acquire_forward_guard(
        &self,
        _handle: &ManagedStorageNamespaceHandleV1,
    ) -> Result<Box<dyn ManagedStorageForwardGuardV1>, ManagedStorageErrorV1> {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    }

    fn preview_control_log_recovery(
        &self,
        _recovery: &ManagedStorageNamespaceHandleV1,
        _old: &ManagedStorageNamespaceHandleV1,
        _successor: &ManagedStorageNamespaceHandleV1,
        _request: ControlLogRecoveryRequestV1,
    ) -> Result<ControlLogRecoveryPreviewV1, ManagedStorageErrorV1> {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    }

    fn advance_control_log_recovery(
        &self,
        _recovery: &ManagedStorageNamespaceHandleV1,
        _old: &ManagedStorageNamespaceHandleV1,
        _successor: &ManagedStorageNamespaceHandleV1,
        _preview: &ControlLogRecoveryPreviewV1,
        _header: &[u8],
    ) -> Result<ControlLogRecoveryStateV1, ManagedStorageErrorV1> {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    }

    fn query_control_log_recovery(
        &self,
        _recovery: &ManagedStorageNamespaceHandleV1,
    ) -> Result<Option<ControlLogRecoveryStateV1>, ManagedStorageErrorV1> {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    }

    /// Reconciles measured bytes/entries for a live storage namespace without exposing the
    /// authority's quota book or physical path. Artifact adapters use this only while holding
    /// their paired staging/store leases; the authority remains the sole quota owner.
    fn reconcile_namespace_quota(
        &self,
        _handle: &ManagedStorageNamespaceHandleV1,
        _bytes: u64,
        _entries: u64,
    ) -> Result<(), ManagedStorageErrorV1> {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    }

    /// Reserves total byte capacity for this exact live namespace before physical writes.
    /// The authority must revalidate the handle, current grant and generation; the caller keeps
    /// the same lease and namespace mutation lock through admission and the physical write.
    /// `minimum_bytes <= preferred_bytes` is required. The returned capacity lies within that
    /// range and may be reduced under quota pressure; `entries` is the exact total entry charge.
    /// This pathless operation leaves quota ownership and durable accounting in the authority.
    ///
    /// # Errors
    /// Fails for an invalid range, stale or mismatched handle, insufficient byte/entry/workspace
    /// quota, unavailable authority, or durable snapshot CAS or persistence failure. Rejected
    /// growth does not release the preceding charge. Uncertain durability must reject further
    /// mutation until the authority reopens and verifies its durable state.
    fn reserve_namespace_quota_capacity(
        &self,
        _handle: &ManagedStorageNamespaceHandleV1,
        _minimum_bytes: u64,
        _preferred_bytes: u64,
        _entries: u64,
    ) -> Result<u64, ManagedStorageErrorV1> {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    }

    /// Releases only the live holder; retained capacity is not proof that bytes were deleted.
    fn detach_namespace(
        &self,
        _handle: ManagedStorageNamespaceHandleV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    }

    fn finalize_namespace(
        &self,
        handle: ManagedStorageNamespaceHandleV1,
        reason: String,
    ) -> Result<ManagedStorageStorageReceiptV1, ManagedStorageErrorV1>;

    /// Re-reads and settles one physical writer frontier as one authority-owned operation.
    /// The authority must hold the physical namespace lock from the re-read through the durable
    /// observation and settlement, so a writer cannot append after proof and before settlement.
    fn finalize_namespace_with_physical_frontier(
        &self,
        handle: ManagedStorageNamespaceHandleV1,
        byte_length: u64,
        record_count: u64,
        content_hash: CanonicalHash,
        reason: String,
    ) -> Result<ManagedStorageStorageReceiptV1, ManagedStorageErrorV1>;
}

/// Closed storage error taxonomy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManagedStorageErrorV1 {
    #[error("storage capability does not match the admission request")]
    CapabilityMismatch,
    #[error("storage capability already consumed (one-shot)")]
    DuplicateClaim,
    #[error("handle was finalized or suspended")]
    HandleFinalized,
    #[error("logical key descriptor contains a path-bearing or unsafe atom")]
    UnsafeLogicalKey,
    #[error("namespace does not permit this capability family")]
    FamilyMismatch,
    #[error("managed storage authority is unavailable")]
    AuthorityUnavailable,
    #[error(
        "managed storage quota exceeded for {dimension:?}: requested={requested} limit={limit}"
    )]
    QuotaExceeded {
        dimension: ManagedStorageQuotaDimensionV1,
        requested: u64,
        limit: u64,
    },
}

/// The authority-owned limit which rejected a physical storage reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedStorageQuotaDimensionV1 {
    Bytes,
    Entries,
    WorkspaceBytes,
}

/// Closed artifact id for the dual-grant publish path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactStoreReferenceV1 {
    pub artifact_id: OpaqueArtifactId,
    pub object_key_hash: CanonicalHash,
    pub publish_receipt_hash: CanonicalHash,
}

/// Reads the physical frontier of a JSONL source with fixed memory usage. This only
/// measures complete record framing; each semantic owner validates the record payloads.
///
/// # Errors
/// Returns an I/O error for an unreadable source, counter overflow, or a missing final
/// record separator. An incomplete source must be preserved for explicit recovery.
pub fn read_jsonl_physical_frontier(
    mut source: impl std::io::Read,
) -> std::io::Result<(u64, u64, CanonicalHash)> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    let mut records = 0_u64;
    let mut line_has_bytes = false;
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .ok_or_else(|| std::io::Error::other("managed source byte length overflow"))?;
        hash.update(&buffer[..count]);
        for byte in &buffer[..count] {
            if *byte == b'\n' {
                if line_has_bytes {
                    records = records.checked_add(1).ok_or_else(|| {
                        std::io::Error::other("managed source record count overflow")
                    })?;
                }
                line_has_bytes = false;
            } else {
                line_has_bytes = true;
            }
        }
    }
    if line_has_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "managed record object ends with an incomplete JSONL separator",
        ));
    }
    Ok((
        bytes,
        records,
        CanonicalHash::from_bytes(hash.finalize().into()),
    ))
}

/// Hashes a physical stream with constant memory, including incomplete journal tails.
///
/// # Errors
/// Returns source I/O errors or an overflowing byte count.
pub fn read_physical_digest(
    mut source: impl std::io::Read,
) -> std::io::Result<(u64, CanonicalHash)> {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    let mut length = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        length = length
            .checked_add(read as u64)
            .ok_or_else(|| std::io::Error::other("physical stream byte count overflow"))?;
        digest.update(&buffer[..read]);
    }
    Ok((length, CanonicalHash::from_bytes(digest.finalize().into())))
}
