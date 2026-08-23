//! RFC-0071 section 7 / 8.1: pathless, provider-neutral resource contract.
//!
//! This module is the only resource authority contract surface in the kernel. It carries no
//! `PathBuf`, mount target, SID, mode bit or backend name: such local facts live only in
//! `sigil-resource-authority` / `sigil-sandbox` implementation layers.
//!
//! Every enum here is a current-schema closed contract: unknown discriminant, missing field,
//! duplicate access or unsorted set is rejected before allocation or hashing. Quota units are
//! bytes, entry count, holder count and milliseconds; u64/u32 use canonical big-endian unsigned
//! encoding. `BorrowedAccountingOnly` requires hard_runtime_enforcement_required=false and
//! cleanup=BorrowedNoCleanup; managed writable kinds must not use that profile.

use std::collections::BTreeSet;
use std::fmt;

pub const RESOURCE_CONTRACT_VERSION: u32 = 1;
pub const MAX_RESOURCE_REQUIREMENTS: usize = 64;
pub const MAX_RESOURCE_GRANTS: usize = 64;

// ---------------------------------------------------------------------------
// Canonical opaque identity primitives
// ---------------------------------------------------------------------------

/// 32-byte canonical digest with stable big-endian unsigned encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalHash([u8; 32]);

impl CanonicalHash {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            use std::fmt::Write as _;
            let _ = write!(&mut out, "{byte:02x}");
        }
        out
    }
}

impl fmt::Display for CanonicalHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl serde::Serialize for CanonicalHash {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> serde::Deserialize<'de> for CanonicalHash {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.len() != 64 {
            return Err(serde::de::Error::custom(
                "canonical hash must be 64 hex chars",
            ));
        }
        let mut bytes = [0u8; 32];
        for (index, chunk) in text.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = u8::from_str_radix(
                std::str::from_utf8(chunk)
                    .map_err(|_| serde::de::Error::custom("invalid hex in canonical hash"))?,
                16,
            )
            .map_err(|_| serde::de::Error::custom("invalid hex in canonical hash"))?;
        }
        Ok(Self(bytes))
    }
}

macro_rules! opaque_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub const fn new(value: String) -> Self {
                Self(value)
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Ok(Self(String::deserialize(deserializer)?))
            }
        }
    };
}

opaque_id!(OpaqueResourceId, "Opaque local resource identity.");
opaque_id!(OpaqueRequirementId, "Opaque resource requirement identity.");
opaque_id!(OpaqueWorkspaceId, "Opaque workspace identity.");
opaque_id!(OpaqueSessionId, "Opaque session identity.");
opaque_id!(OpaqueRunId, "Opaque run identity.");
opaque_id!(OpaqueTerminalTaskId, "Opaque terminal task identity.");
opaque_id!(OpaqueExtensionId, "Opaque extension identity.");
opaque_id!(
    OpaquePublishTransactionId,
    "Opaque artifact publish transaction identity."
);
opaque_id!(OpaqueArtifactId, "Opaque artifact identity.");
opaque_id!(
    OpaqueRetentionPolicyId,
    "Opaque semantic retention policy identity."
);
opaque_id!(OpaqueBlockerId, "Opaque recovery blocker identity.");
opaque_id!(
    OpaqueResolutionAttemptId,
    "Opaque resolution attempt identity."
);
opaque_id!(
    OpaqueRecoveryOperationId,
    "Opaque recovery operation identity."
);
opaque_id!(
    OpaqueExecutionPlanDraftId,
    "Opaque execution plan draft identity."
);
opaque_id!(
    OpaqueManagedStoragePlanId,
    "Opaque managed storage plan identity."
);
opaque_id!(
    OpaqueStorageOperationAttemptId,
    "Opaque storage operation attempt identity."
);
opaque_id!(
    OpaqueManagedFileAccessPlanId,
    "Opaque managed file access plan identity."
);
opaque_id!(
    OpaquePermissionSubjectRef,
    "Opaque permission subject reference."
);
opaque_id!(OpaqueAdmissionId, "Opaque approved admission identity.");
opaque_id!(OpaqueArtifactRefV1, "Opaque artifact reference.");
opaque_id!(OpaqueProcessRef, "Opaque process reference.");
opaque_id!(
    OpaqueSessionControllerInstanceIdV1,
    "Opaque session controller instance identity."
);
opaque_id!(OpaqueSandboxBinderId, "Opaque sandbox binder identity.");
opaque_id!(
    OpaqueSandboxProviderRecoveryLineageId,
    "Opaque sandbox provider recovery lineage identity."
);
opaque_id!(OpaqueSelectionNonce, "Opaque user selection nonce.");
opaque_id!(OpaqueSessionExportId, "Opaque session export identity.");
opaque_id!(
    OpaqueRegistrationCapsuleId,
    "Opaque registration capsule identity."
);
opaque_id!(OpaqueServerInstanceId, "Opaque server instance identity.");
opaque_id!(
    OpaqueSandboxSpawnRecoveryBatchRequestId,
    "Opaque spawn recovery batch request identity."
);
opaque_id!(
    OpaqueBorrowedMutationRecoveryAttemptId,
    "Opaque borrowed mutation recovery attempt identity."
);
opaque_id!(
    OpaqueNativeDialogSelectionId,
    "Opaque native dialog selection identity."
);

/// Pathless process reference visible to consumers (never carries OS handle).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ReflectiveOpaqueProcessRef(OpaqueProcessRef);

impl ReflectiveOpaqueProcessRef {
    pub const fn new(value: OpaqueProcessRef) -> Self {
        Self(value)
    }
}

opaque_id!(
    OpaquePermissionDecisionId,
    "Opaque permission decision identity."
);
opaque_id!(OpaqueApprovalRequestId, "Opaque approval request identity.");
opaque_id!(OpaqueToolCallId, "Opaque tool call identity.");
opaque_id!(OpaqueSessionGrantRef, "Opaque session grant reference.");
opaque_id!(OpaqueStorageKeyIdV1, "Opaque storage logical key identity.");
opaque_id!(OpaqueStorageGrantId, "Opaque storage grant identity.");
opaque_id!(OpaqueDomainEventId, "Opaque domain event identity.");
opaque_id!(
    OpaqueStorageNamespaceId,
    "Opaque storage namespace identity."
);
opaque_id!(
    OpaqueKernelProofHandleId,
    "Opaque kernel proof handle identity."
);
opaque_id!(
    OpaqueKernelCapabilityHandleId,
    "Opaque kernel capability handle identity."
);
opaque_id!(
    OpaqueKernelProofAuthenticatorV1,
    "Opaque kernel proof authenticator."
);
opaque_id!(
    OpaqueKernelCapabilityAuthenticatorV1,
    "Opaque kernel capability authenticator."
);
opaque_id!(OpaqueStagedBlobRef, "Opaque staged blob reference.");
opaque_id!(OpaqueBlobWriterId, "Opaque blob writer identity.");
opaque_id!(OpaqueSemanticSchemaId, "Opaque semantic schema identity.");
opaque_id!(
    OpaqueMutationOperationId,
    "Opaque mutation operation identity."
);
opaque_id!(OpaqueMutationBatchId, "Opaque mutation batch identity.");
opaque_id!(
    OpaqueBorrowedFileSubjectRef,
    "Opaque borrowed file subject reference."
);
opaque_id!(OpaqueSpawnIntentId, "Opaque spawn intent identity.");

/// Physical attempt identity: one actual execution attempt (spawn) of a logical tool call.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct PhysicalAttemptId(String);

impl PhysicalAttemptId {
    pub const fn new(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PhysicalAttemptId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Extension kind used by owner scopes and blocker scopes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ExtensionKindV1 {
    McpStdio,
    Plugin,
}

// ---------------------------------------------------------------------------
// BoundedVec
// ---------------------------------------------------------------------------

/// Bounded collection with a schema-level maximum. All durable/public/request/result
/// collections use this instead of unbounded `Vec`: decode, canonical hashing and projection
/// run the same limit check before allocation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BoundedVec<T, const MAX: usize>(Vec<T>);

impl<T, const MAX: usize> BoundedVec<T, MAX> {
    pub const fn max() -> usize {
        MAX
    }

    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn try_push(&mut self, value: T) -> Result<(), ResourceContractError> {
        if self.0.len() >= MAX {
            return Err(ResourceContractError::BoundedVecExceeded { max: MAX });
        }
        self.0.push(value);
        Ok(())
    }

    pub fn try_from_vec(values: Vec<T>) -> Result<Self, ResourceContractError> {
        if values.len() > MAX {
            return Err(ResourceContractError::BoundedVecExceeded { max: MAX });
        }
        Ok(Self(values))
    }

    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn into_inner(self) -> Vec<T> {
        self.0
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.0.iter()
    }
}

impl<T, const MAX: usize> Default for BoundedVec<T, MAX> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const MAX: usize> FromIterator<T> for BoundedVec<T, MAX> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let values = iter.into_iter().collect::<Vec<_>>();
        Self(values)
    }
}

impl<T: serde::Serialize, const MAX: usize> serde::Serialize for BoundedVec<T, MAX> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de, T: serde::Deserialize<'de>, const MAX: usize> serde::Deserialize<'de>
    for BoundedVec<T, MAX>
{
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = Vec::<T>::deserialize(deserializer)?;
        Self::try_from_vec(values).map_err(|error| serde::de::Error::custom(error.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Resource kinds and categories (section 7.1)
// ---------------------------------------------------------------------------

/// The closed resource kind taxonomy.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ResourceKindV1 {
    Workspace,
    ExecutionTemp,
    SessionScratch,
    RuntimeState,
    RuntimeCache,
    ArtifactStaging,
    ArtifactStore,
    IsolatedWorkspace,
    ToolchainStore,
    ToolCache,
    UserConfig,
    ExternalUserPath,
    SystemTemp,
}

impl ResourceKindV1 {
    pub const fn is_managed(self) -> bool {
        matches!(
            self,
            Self::ExecutionTemp
                | Self::SessionScratch
                | Self::RuntimeState
                | Self::RuntimeCache
                | Self::ArtifactStaging
                | Self::ArtifactStore
                | Self::IsolatedWorkspace
                | Self::ToolCache
        )
    }
}

/// Lease lifetime: who holds access/admission, not physical retention.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ResourceLeaseLifetimeV1 {
    ToolCall,
    TerminalTask,
    ExtensionProcess,
    Run,
    Session,
    Workspace,
    Application,
    PublishTransaction,
}

/// Physical retention decision; never derived from lease lifetime or directory names.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ResourceRetentionPolicyV1 {
    /// Release the exact generation on settlement.
    ReleaseOnSettlement,
    /// Session retention policy decides.
    SessionPolicy,
    /// Workspace cache policy decides.
    WorkspaceCachePolicy,
    /// A durable semantic owner policy decides.
    DurableSemanticOwnerPolicy(OpaqueRetentionPolicyId),
    /// Borrowed resource: never deleted by the authority.
    BorrowedNoCleanup,
}

/// Access grants; each entry is a distinct authority decision.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ResourceAccessV1 {
    Read,
    Write,
    Create,
    /// Delete only entries managed by the authority in the leased generation.
    DeleteManaged,
    /// Delete an exact subject (borrowed resource, subject-bound).
    DeleteExactSubject,
    /// Delete a subject subtree (higher risk, needs confirmation).
    DeleteSubjectSubtree,
    /// Rename / atomic replace within the granted subject pair.
    RenameWithinGrant,
    Execute,
}

/// Why a resource is required (semantic purpose).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ResourcePurposeV1 {
    ExecutionPrerequisite,
    PersistentScratch,
    SemanticRuntimeState,
    RebuildableCache,
    ExecutionCapture,
    ArtifactPublish,
    IsolatedWorkspace,
    ToolchainResolution,
    ToolCache,
    SafeConfigurationProjection,
    BorrowedFileOperation,
    ResourceRecovery,
    HostDiagnostic,
}

/// How the resource is visible to the model / to child processes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ResourceVisibilityV1 {
    HostOnly,
    ExactChildGrant,
    SafeLabelOnly,
    OpaqueArtifactReference,
    SanitizedProjection,
}

/// Quota accounting class.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ResourceQuotaClassV1 {
    AttemptEphemeral,
    SessionScratch,
    RuntimeState,
    RuntimeCache,
    ArtifactStaging,
    ArtifactStore,
    IsolatedWorkspace,
    ToolCache,
    BorrowedAccountingOnly,
    Quarantine,
}

/// Closed quota profile. Units are bytes / entries / holders / milliseconds.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ResourceQuotaProfileV1 {
    pub class: ResourceQuotaClassV1,
    pub max_bytes: u64,
    pub max_entries: u64,
    pub max_open_holders: u32,
    pub max_age_ms: Option<u64>,
    pub hard_runtime_enforcement_required: bool,
    pub profile_hash: CanonicalHash,
}

/// Cleanup classification for the generation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ResourceCleanupPolicyV1 {
    ReleaseExactGenerationOnSettlement,
    RetainBySemanticOwner,
    QuarantineExactGenerationOnFailure,
    RetainUntilSessionLifecycle,
    RetainUntilWorkspaceCachePolicy,
    BorrowedNoCleanup,
}

/// Terminal cleanup status of one generation.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ResourceCleanupStatusV1 {
    NotStarted,
    Released,
    RetainedByPolicy,
    Quarantined { quarantine_ref: OpaqueResourceId },
    CleanupIncomplete { evidence_digest: CanonicalHash },
    NotApplicableBorrowed,
}

/// Environment profile class for an execution attempt.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum EnvironmentProfileClassV1 {
    FreshIsolatedHome,
    FreshIsolatedHomeWithToolchain,
    WorkspaceBound,
    PersistentTerminal,
    ExtensionProcess,
    ExplicitUnconfined,
}

/// Toolchain binding class for resolver/materializer decisions.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ToolchainBindingClassV1 {
    Rust,
    Node,
    Git,
    GenericExecutable,
}

/// Sandbox backend class; a truthful observation, never a request.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum SandboxBackendClassV1 {
    MacOsSeatbelt,
    LinuxBubblewrap,
    Docker,
    WindowsRestricted,
    LocalUnconfined,
}

/// Enforcement requirement class.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum EnforcementRequirementClassV1 {
    RequiredExact,
    RequiredDeclaredSuperset { declaration_hash: CanonicalHash },
    Preferred,
    ExplicitUnconfined,
}

/// Requested enforcement (side-effect-free, entered into the plan hash).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct RequestedEnforcementV1 {
    pub requirement: EnforcementRequirementClassV1,
    pub deny_ambient_system_temp_write: bool,
    pub deny_ambient_home_write: bool,
    pub deny_ungranted_workspace_write: bool,
    pub require_process_tree_ownership: bool,
    pub require_network_policy: bool,
    pub requested_capability_set_hash: CanonicalHash,
    pub profile_hash: CanonicalHash,
}

/// Observed enforcement completeness (backend truth, never copied from request).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum EnforcementCompletenessV1 {
    Exact,
    Partial,
    None,
}

/// Effective enforcement observation from backend probe/receipt.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct EffectiveEnforcementV1 {
    pub backend: SandboxBackendClassV1,
    pub completeness: EnforcementCompletenessV1,
    pub effective_capability_set_hash: CanonicalHash,
    pub access_widening_set_hash: CanonicalHash,
    pub functional_probe_hash: CanonicalHash,
    pub proof_set_hash: CanonicalHash,
}

/// Managed arena class.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ManagedArenaClassV1 {
    State,
    Cache,
    ExecutionTemp,
    Artifact,
    IsolatedWorkspace,
    ToolCache,
}

/// Holder kind for lease accounting.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum HolderKindV1 {
    ExecutionAttempt,
    ProcessTree,
    SemanticOwner,
    StoragePrimitive,
    BlobWriter,
    ProjectionConnection,
    WorkspaceMutationLease,
    SessionWriterAttachment,
    ResourceMaintenance,
    StartupReconciler,
}

/// Evidence class proven by a receipt.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ResourceEvidenceClassV1 {
    BootstrapRoot,
    Identity,
    OwnerOrAcl,
    Quota,
    Journal,
    Sandbox,
    AliasContainment,
    Storage,
    ProcessFrontier,
    Cleanup,
}

/// Owner scope for physical ownership of one requirement.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ResourceOwnerScopeV1 {
    Application,
    Workspace(OpaqueWorkspaceId),
    Session(OpaqueSessionId),
    Run(OpaqueRunId),
    PhysicalAttempt(PhysicalAttemptId),
    TerminalTask(OpaqueTerminalTaskId),
    ExtensionProcess {
        extension_kind: ExtensionKindV1,
        extension_id: OpaqueExtensionId,
        generation: u64,
    },
    PublishTransaction(OpaquePublishTransactionId),
    Artifact(OpaqueArtifactId),
}

/// Blocker scope for stable cross-call identity of a resource requirement.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ResourceBlockerScopeV1 {
    Application,
    Workspace(OpaqueWorkspaceId),
    Session(OpaqueSessionId),
    Run(OpaqueRunId),
    TerminalTask(OpaqueTerminalTaskId),
    Extension {
        extension_kind: ExtensionKindV1,
        extension_id: OpaqueExtensionId,
        config_generation: u64,
    },
    PublishTransaction(OpaquePublishTransactionId),
    Artifact(OpaqueArtifactId),
}

/// Journal shard selection for one attempt.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ResourceJournalScopeV1 {
    Application,
    Workspace(OpaqueWorkspaceId),
}

// ---------------------------------------------------------------------------
// Requirement model (section 8.1)
// ---------------------------------------------------------------------------

/// Immutable reference to one realized resource generation.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ResourceRefV1 {
    pub resource_id: OpaqueResourceId,
    pub kind: ResourceKindV1,
    pub owner_scope: ResourceOwnerScopeV1,
    pub journal_scope: ResourceJournalScopeV1,
    pub generation: u64,
}

/// Pathless logical requirement for one resource.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ResourceRequirementV1 {
    pub requirement_id: OpaqueRequirementId,
    pub physical_owner_scope: ResourceOwnerScopeV1,
    pub stable_key: ResourceRequirementKeyV1,
    pub kind: ResourceKindV1,
    pub lease_lifetime: ResourceLeaseLifetimeV1,
    pub access: BTreeSet<ResourceAccessV1>,
    pub purpose: ResourcePurposeV1,
    pub visibility: ResourceVisibilityV1,
    pub quota_profile: ResourceQuotaProfileV1,
    pub retention_policy: ResourceRetentionPolicyV1,
    pub cleanup_policy: ResourceCleanupPolicyV1,
    pub implicit: bool,
}

/// Stable logical identity of one requirement across calls.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ResourceRequirementKeyV1 {
    pub blocker_scope: ResourceBlockerScopeV1,
    pub kind: ResourceKindV1,
    pub purpose: ResourcePurposeV1,
    pub access: BTreeSet<ResourceAccessV1>,
    pub lease_lifetime: ResourceLeaseLifetimeV1,
    pub quota_profile: ResourceQuotaProfileV1,
    pub retention_policy: ResourceRetentionPolicyV1,
    pub cleanup_policy: ResourceCleanupPolicyV1,
    pub environment_class: EnvironmentProfileClassV1,
    pub toolchain_class: Option<ToolchainBindingClassV1>,
    pub subject_binding_hash: Option<CanonicalHash>,
    pub canonical_hash: CanonicalHash,
}

/// The complete pathless resource requirement set for one plan/attempt.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ResourceRequirementSetV1 {
    pub schema_version: u32,
    pub requirements: BoundedVec<ResourceRequirementV1, MAX_RESOURCE_REQUIREMENTS>,
    pub canonical_hash: CanonicalHash,
}

/// Contract validation error; closed and stable (never an ad-hoc string).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResourceContractError {
    #[error("bounded collection exceeded its schema maximum of {max}")]
    BoundedVecExceeded { max: usize },
    #[error("resource access set must be sorted and duplicate-free")]
    DuplicateAccess,
    #[error(
        "borrowed accounting quota profile requires no hard runtime enforcement and borrowed-no-cleanup"
    )]
    InvalidBorrowedQuotaProfile,
    #[error("managed writable kind cannot use BorrowedAccountingOnly quota class")]
    InvalidManagedQuotaProfile,
    #[error("requirement contains a path-backed field, which the kernel contract forbids")]
    PathBackedField,
    #[error("requirement set is not in canonical (stable) order")]
    NonCanonicalOrder,
    #[error("requirement references an unknown resource contract version")]
    UnknownContractVersion,
    #[error("V3 plan is not one of the four closed envelope shapes")]
    InvalidV3EnvelopeShape,
    #[error("V3 decision references a hash that does not match the approved plan")]
    V3DecisionMismatch,
}

impl ResourceRequirementKeyV1 {
    /// Validates closed-set invariants without hashing.
    pub fn validate(&self) -> Result<(), ResourceContractError> {
        let mut previous: Option<ResourceAccessV1> = None;
        let mut seen = BTreeSet::new();
        for access in &self.access {
            if !seen.insert(*access) {
                return Err(ResourceContractError::DuplicateAccess);
            }
            if previous.is_some_and(|previous| *access <= previous) {
                return Err(ResourceContractError::NonCanonicalOrder);
            }
            previous = Some(*access);
        }
        Ok(())
    }
}

impl ResourceRequirementV1 {
    /// Contract precondition check: managed-writable kinds never use the borrowed accounting
    /// profile; borrowed kinds never claim hard runtime enforcement.
    pub fn validate(&self) -> Result<(), ResourceContractError> {
        self.stable_key.validate()?;
        let borrowed_profile =
            self.quota_profile.class == ResourceQuotaClassV1::BorrowedAccountingOnly;
        if borrowed_profile {
            if self.quota_profile.hard_runtime_enforcement_required
                || self.cleanup_policy != ResourceCleanupPolicyV1::BorrowedNoCleanup
            {
                return Err(ResourceContractError::InvalidBorrowedQuotaProfile);
            }
            if self.kind.is_managed()
                && matches!(
                    self.kind,
                    ResourceKindV1::ExecutionTemp
                        | ResourceKindV1::SessionScratch
                        | ResourceKindV1::ArtifactStaging
                        | ResourceKindV1::ArtifactStore
                        | ResourceKindV1::IsolatedWorkspace
                        | ResourceKindV1::ToolCache
                        | ResourceKindV1::RuntimeState
                        | ResourceKindV1::RuntimeCache
                )
            {
                return Err(ResourceContractError::InvalidManagedQuotaProfile);
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Storage / authority support types referenced by V3 plans (section 8.2 dependencies)
// ---------------------------------------------------------------------------

/// Authority generation: epoch plus instance digest. Enters every admission binding.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct AuthorityGeneration {
    pub epoch: u64,
    pub instance_hash: CanonicalHash,
}

/// Closed storage capability families.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ManagedStorageCapabilityFamilyV1 {
    AppendLog,
    AtomicObject,
    JournaledAtomicProjection,
    StreamingArtifact,
    ArtifactStore,
    RebuildableDatabaseProjection,
    SemanticLeaseLedger,
}

/// Closed semantic owner classes for managed storage namespaces.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ManagedStorageSemanticOwnerV1 {
    SessionLog,
    SessionLifecycleLog,
    InteractiveInputHistory,
    DurableMemory(MemoryScopeClassV1),
    WorkspaceMutationState,
    ApplicationControlLog,
    PlanStore,
    SessionCatalog,
    ProviderConnectionState,
    AdapterDurableState(AdapterDurableStateClassV1),
    RuntimeCache(CacheOwnerClassV1),
    ArtifactStaging,
    ArtifactStore,
}

/// Closed adapter durable-state classes (transport-neutral).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum AdapterDurableStateClassV1 {
    ProtocolReplay,
    EgressDisclosure,
    IdempotencyLedger,
}

/// Closed cache-owner classes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum CacheOwnerClassV1 {
    ProviderCatalog,
    TokenizerProfile,
    ModelMetadata,
    CodeIntelligence,
}

/// Closed durable-memory scope classes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum MemoryScopeClassV1 {
    UserPreference,
    ProjectFact,
}

/// Closed admission source classes for storage.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum StorageAdmissionSourceClassV1 {
    ApplicationCutoverRoot,
    ApplicationControlReady,
    ApplicationLifecycleReady,
    SessionLifecycle,
    WorkspaceLifecycle,
    ToolDecisionExecution,
    ToolDecisionInProcessStorage,
    ExtensionDecision,
    SemanticTransaction,
    RecoveryAction,
}

/// Closed storage admission purpose classes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ManagedStorageAdmissionPurposeV1 {
    DurablePayload,
    RebuildableProjection,
    ArtifactPublish,
    SemanticLease,
    CacheRefresh,
    RecoveryReconcile,
}

/// Closed snapshot coverage class for RFC-0002 mutation plans.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum SnapshotCoverageV1 {
    Captured,
    NoPriorState,
    SensitiveOmitted,
}

/// Resource usage snapshot (closed units: bytes, entries, holders).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ResourceUsageV1 {
    pub logical_bytes: u64,
    pub physical_bytes: u64,
    pub entry_count: u64,
    pub open_holder_count: u64,
}

/// One-shot execution admission bundle (private consumer token + sibling resource capability).
/// The variants are non-clone / non-serialize; the kernel broker is the only constructor.
#[derive(Debug)]
pub enum IssuedExecutionAdmissionBundleV1 {
    OneShot {
        consumer_token: OpaqueResourceId,
        resource_capability: OpaqueResourceId,
    },
    Terminal {
        consumer_token: OpaqueResourceId,
        resource_capability: OpaqueResourceId,
    },
    Extension {
        consumer_token: OpaqueResourceId,
        resource_capability: OpaqueResourceId,
    },
}
