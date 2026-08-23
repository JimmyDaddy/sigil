//! RFC-0071: RA-owned spawn protocol capsule (contract-only, R71.1).
//!
//! R71.1 provides the shape and the sealed constructor boundary only; no platform/filesystem
//! mutation exists yet (that is R71.2/R71.3). The purpose is to freeze the crate ownership
//! direction: sigil-sandbox never constructs, clones, or serializes the prepared launch.

use sigil_kernel::resource::{CanonicalHash, OpaqueResourceId};

/// Non-clone lease carrier exchanged between authority and sandbox binder.
#[derive(Debug)]
pub struct ExecutionResourceLeasePrivate;

/// RA-owned sealed launch capsule. Fields are private and there is no into_parts:
/// runtime cannot split pending state, protocol request and sink.
pub struct SandboxBoundExecutionLeaseV1 {
    #[allow(dead_code)]
    binder_id: OpaqueResourceId,
    #[allow(dead_code)]
    provider_recovery_lineage_id: OpaqueResourceId,
    #[allow(dead_code)]
    pending_launch_hash: CanonicalHash,
    // NonClone owner held by the authority registry.
    #[allow(dead_code)]
    _holder: Option<ExecutionResourceLeasePrivate>,
}

impl SandboxBoundExecutionLeaseV1 {
    /// The only constructor site for the sealed capsule. Placeholder pending R71.3: production
    /// implementation verifies factory attestation against the activated provider registry before
    /// the pending actor and sink become usable.
    pub fn issue_prepared_launch(
        self,
        _evidence: SandboxPendingLaunchFactoryEvidenceV1,
        _initiation_sink: std::boxed::Box<dyn SandboxInitiatedSpawnBundleSinkV1>,
    ) -> Result<PreparedSandboxLaunchV1, SandboxLaunchErrorV1> {
        Err(SandboxLaunchErrorV1::ContractOnly)
    }
}

/// Evidence submitted by the sandbox factory (contract shape).
#[derive(Debug, Clone)]
pub struct SandboxPendingLaunchFactoryEvidenceV1 {
    pub physical_attempt_id: sigil_kernel::resource::PhysicalAttemptId,
    pub spawn_intent_id: sigil_kernel::resource::OpaqueSpawnIntentId,
    pub provider_registration_hash: CanonicalHash,
}

/// One-shot sink that accepts the initiated journal bundle without a rejected branch.
pub trait SandboxInitiatedSpawnBundleSinkV1: Send {
    fn accept_initiated_bundle(
        self: Box<Self>,
        initiated: InitiatedSpawnJournalBundleV1,
    ) -> SpawnSupervisorAcceptedTicketV1;
}

/// Accepted ticket returned after the whole bundle moved into the reserved root mailbox.
#[derive(Debug, Clone)]
pub struct SpawnSupervisorAcceptedTicketV1 {
    pub ticket_hash: CanonicalHash,
}

/// Initiated bundle aggregate consumed by the sink.
#[derive(Debug)]
pub struct InitiatedSpawnJournalBundleV1;

/// Prepared capsule handed to the coordinator (borrowed protocol request only).
pub struct PreparedSandboxLaunchV1 {
    #[allow(dead_code)]
    bound_lease: SandboxBoundExecutionLeaseV1,
    protocol_request: ResourceSpawnProtocolRequestV1,
    #[allow(dead_code)]
    initiation_sink: std::boxed::Box<dyn SandboxInitiatedSpawnBundleSinkV1>,
}

impl PreparedSandboxLaunchV1 {
    pub fn protocol_request(&self) -> &ResourceSpawnProtocolRequestV1 {
        &self.protocol_request
    }
}

/// Provider-neutral, pathless spawn protocol request.
#[derive(Debug, Clone)]
pub struct ResourceSpawnProtocolRequestV1 {
    pub physical_attempt_id: sigil_kernel::resource::PhysicalAttemptId,
    pub spawn_intent_id: sigil_kernel::resource::OpaqueSpawnIntentId,
    pub launch_plan_hash: CanonicalHash,
    pub sandbox_binding_hash: CanonicalHash,
}

/// Closed launch error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SandboxLaunchErrorV1 {
    #[error("sandbox launch protocol is contract-only until R71.3")]
    ContractOnly,
    #[error("provider factory attestation mismatch: {0}")]
    FactoryAttestationMismatch(String),
    #[error("sandbox provider registration unavailable: {0}")]
    ProviderUnavailable(String),
}
