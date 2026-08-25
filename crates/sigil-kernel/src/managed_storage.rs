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
    OpaqueStorageGrantId, OpaqueStorageKeyIdV1, ResourceJournalScopeV1, ResourceKindV1,
    ResourceOwnerScopeV1, ResourceQuotaProfileV1, ResourceRefV1, ResourceRetentionPolicyV1,
    StorageAdmissionSourceClassV1, StorageLogicalKeyKindV1,
};

pub const MAX_STORAGE_LOGICAL_KEY_ATOMS: usize = 8;

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
    pub journal_scope: ResourceJournalScopeV1,
    pub journal_scope_hash: CanonicalHash,
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
    pub journal_admission_sequence: u64,
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
    pub journal_scope: ResourceJournalScopeV1,
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
    pub journal_scope: ResourceJournalScopeV1,
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

/// Reduced storage receipt (full contract lives with the authority journal).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedStorageStorageReceiptV1 {
    pub grant_id: OpaqueStorageGrantId,
    pub grant_hash: CanonicalHash,
    pub semantic_owner: ManagedStorageSemanticOwnerV1,
    pub capability_family: ManagedStorageCapabilityFamilyV1,
    pub resource_id: OpaqueResourceId,
    pub operation_digest: CanonicalHash,
    pub committed_sequence_or_version: Option<u64>,
    pub committed_frontier_hash: CanonicalHash,
    pub receipt_hash: CanonicalHash,
    /// Authority-owned binding to the exact physical bytes used for normal settlement. Probe
    /// receipts and isolated in-memory authority receipts may leave these absent.
    #[serde(default)]
    pub physical_frontier_hash: Option<CanonicalHash>,
    #[serde(default)]
    pub physical_observation_record_hash: Option<CanonicalHash>,
}

/// Consumer facing pathless managed storage service (authority implementation).
pub trait ManagedStorageServiceV1: Send + Sync {
    fn admit_namespace(
        &self,
        request: ManagedStorageAdmissionRequestV1,
        capability: ValidatedStorageAdmissionCapabilityV1,
    ) -> Result<ManagedStorageNamespaceHandleV1, ManagedStorageErrorV1>;

    /// Validates that a physical mutation is still covered by an admitted namespace. The
    /// runtime calls this while holding the namespace lock, so settlement wins the race before a
    /// post-settlement write can reach the physical object.
    fn validate_namespace_write(
        &self,
        handle: &ManagedStorageNamespaceHandleV1,
    ) -> Result<(), ManagedStorageErrorV1>;

    /// Reconciles measured bytes/entries for a live storage namespace without exposing the
    /// authority's quota book or physical path. Artifact adapters use this only while holding
    /// their paired staging/store leases; the authority remains the sole quota owner.
    fn reconcile_namespace_quota(
        &self,
        _handle: &ManagedStorageNamespaceHandleV1,
        _bytes: u64,
        _entries: u64,
    ) -> Result<(), ManagedStorageErrorV1> {
        Err(ManagedStorageErrorV1::JournalUnavailable)
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
    #[error("managed storage authority journal is unavailable")]
    JournalUnavailable,
}

/// Closed artifact id for the dual-grant publish path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactStoreReferenceV1 {
    pub artifact_id: OpaqueArtifactId,
    pub object_key_hash: CanonicalHash,
    pub publish_receipt_hash: CanonicalHash,
}
