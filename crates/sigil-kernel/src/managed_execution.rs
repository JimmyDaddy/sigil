//! RFC-0071 section 8.4: managed execution port, request types and receipts.
//!
//! These are the kernel pathsless, provider-neutral consumer ports. Implementation lives in
//! sigil-runtime holding the authority and sandbox; non-serialized physical leases never cross
//! this port surface.

use std::collections::BTreeSet;
use std::ffi::OsString;

use serde::{Deserialize, Serialize};

use crate::recovery::EffectSettlementV1;
use crate::resource::{
    AuthorityGeneration, CanonicalHash, EffectiveEnforcementV1, EnforcementCompletenessV1,
    OpaquePermissionSubjectRef, PhysicalAttemptId, ReflectiveOpaqueProcessRef,
    RequestedEnforcementV1, ResourceAccessV1, ResourceCleanupStatusV1, ResourceJournalScopeV1,
    ResourceRefV1, ResourceRequirementSetV1,
};

/// Execution purpose for one planned attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionPurposeV1 {
    OneShot,
    Terminal,
    ExtensionProcess,
    CodeIntelProcess,
}

/// Capture policy for stdout/stderr/PTY.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCapturePolicy {
    pub stdout_capture: CaptureModeV1,
    pub stderr_capture: CaptureModeV1,
    pub pty: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureModeV1 {
    None,
    BoundedRing { max_bytes: u64 },
    ArtifactStreaming,
}

/// Bounded execution resource limits (approval input, not free parameters).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionResourceLimits {
    pub max_output_bytes: u64,
    pub max_runtime_ms: u64,
    pub max_children: u32,
    pub max_fds: u32,
    pub pty_required: bool,
}

/// Pathless environment profile reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentProfileRefV1 {
    pub profile_class: crate::resource::EnvironmentProfileClassV1,
    pub profile_hash: CanonicalHash,
}

/// Closed bound on managed execution agreement environment entries (never unbounded envs).
pub const MAX_MANAGED_EXECUTION_ENV_ENTRIES: usize = 128;

/// Closed bound on agreed argv entries (the plan is never an unbounded argv).
pub const MAX_MANAGED_EXECUTION_ARGV_ENTRIES: usize = 64;

/// Closed bound on total agreed argv bytes (bounded agreement, never host-sized payloads).
pub const MAX_MANAGED_EXECUTION_ARGV_BYTES: usize = 64 * 1024;

/// Exact total of argv encoded bytes (closed bound check helper).
pub fn argv_encoded_bytes(argv: &[OsString]) -> usize {
    argv.iter()
        .map(|arg| arg.as_os_str().as_encoded_bytes().len())
        .sum()
}

/// Canonical environment digest: exact key=value bytes, sorted by key. Planner and sandbox
/// recompute the same digest so environment is planner-authoritative (the sandbox never
/// accepts an unagreed environment, and a drift fails closed).
pub fn canonical_environment_digest(environment: &[(OsString, OsString)]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut entries: Vec<(Vec<u8>, Vec<u8>)> = environment
        .iter()
        .map(|(key, value)| {
            (
                key.as_os_str().as_encoded_bytes().to_vec(),
                value.as_os_str().as_encoded_bytes().to_vec(),
            )
        })
        .collect();
    entries.sort();
    let mut hasher = Sha256::new();
    hasher.update(b"managed-execution-env-v1");
    for (key, value) in &entries {
        hasher.update(key);
        hasher.update(b"=");
        hasher.update(value);
        hasher.update(b"\0");
    }
    CanonicalHash::from_bytes(hasher.finalize().into())
}

/// Pre-permission planner request (pathless).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedExecutionPlanRequestV1 {
    pub argv: Vec<OsString>,
    pub cwd_subject_ref: OpaquePermissionSubjectRef,
    pub purpose: ExecutionPurposeV1,
    pub structured_command_digest: CanonicalHash,
    pub owner_scope: crate::resource::ResourceOwnerScopeV1,
    pub capture: ExecutionCapturePolicy,
    pub limits: ExecutionResourceLimits,
    /// Agreed environment (config-granted extension/server env); sealed by the planner.
    pub environment: Vec<(OsString, OsString)>,
}

impl ManagedExecutionPlanRequestV1 {
    pub fn argv_bytes(&self) -> Vec<Vec<u8>> {
        self.argv
            .iter()
            .map(|arg| arg.as_os_str().to_string_lossy().as_bytes().to_vec())
            .collect()
    }
}

/// Pathless planner draft result (side-effect-free).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedExecutionPlanDraftV1 {
    pub draft_id: crate::resource::OpaqueExecutionPlanDraftId,
    pub argv_digest: CanonicalHash,
    pub structured_command_digest: CanonicalHash,
    pub cwd_subject_binding_hash: CanonicalHash,
    pub attempt_journal_scope: ResourceJournalScopeV1,
    pub attempt_journal_scope_hash: CanonicalHash,
    pub resource_plan_hash: CanonicalHash,
    pub resource_requirements: ResourceRequirementSetV1,
    pub environment_profile: EnvironmentProfileRefV1,
    pub toolchain_plan_hash: CanonicalHash,
    pub resolver_proof_digest: CanonicalHash,
    pub sandbox_preview_hash: CanonicalHash,
    pub sandbox_binder_registration_hash: CanonicalHash,
    pub sandbox_provider_generation: u64,
    pub capture_policy_hash: CanonicalHash,
    pub resource_limits_hash: CanonicalHash,
    /// Digest of the agreed environment (see canonical_environment_digest).
    pub environment_digest: CanonicalHash,
    pub draft_hash: CanonicalHash,
}

pub trait ManagedExecutionPlannerV1: Send + Sync {
    fn plan_execution(
        &self,
        request: ManagedExecutionPlanRequestV1,
    ) -> Result<ManagedExecutionPlanDraftV1, ManagedExecutionPlanErrorV1>;
}

/// Closed planner error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManagedExecutionPlanErrorV1 {
    #[error("argv digest drifted from the resolved executable identity")]
    ArgvDigestDrift,
    #[error("resource plan is not side-effect-free: {0}")]
    ResourcePlanSideEffect(String),
    #[error("toolchain resolver proof is stale; replan required")]
    StaleResolverProof,
    #[error("sandbox preview cannot be produced for this request")]
    SandboxPreviewUnavailable,
}

/// Approved admission issued by the continuity validator (not consumer-constructible).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedExecutionAdmissionV1 {
    pub admission_id: crate::resource::OpaqueAdmissionId,
    pub physical_attempt_id: PhysicalAttemptId,
    pub authority_generation: AuthorityGeneration,
    pub resource_plan_hash: CanonicalHash,
    pub execution_plan_draft_hash: CanonicalHash,
    pub argv_digest: CanonicalHash,
    pub structured_command_digest: CanonicalHash,
    pub cwd_subject_binding_hash: CanonicalHash,
    pub attempt_journal_scope: ResourceJournalScopeV1,
    pub attempt_journal_scope_hash: CanonicalHash,
    pub permission_plan_hash: CanonicalHash,
    pub decision_digest: CanonicalHash,
    pub approval_continuity_hash: CanonicalHash,
    pub execution_start_event_digest: CanonicalHash,
    pub requirement_set_hash: CanonicalHash,
    pub subject_binding_hash: CanonicalHash,
    pub requested_enforcement_hash: CanonicalHash,
    pub resolver_proof_digest: CanonicalHash,
    pub sandbox_preview_hash: CanonicalHash,
    pub sandbox_binder_registration_hash: CanonicalHash,
    pub sandbox_provider_generation: u64,
    pub capture_policy_hash: CanonicalHash,
    pub resource_limits_hash: CanonicalHash,
    pub resource_requirements: ResourceRequirementSetV1,
    pub requested_enforcement: RequestedEnforcementV1,
    pub admission_hash: CanonicalHash,
}

/// Post-decision execution request (all fields are approval-bound).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedExecutionRequestV1 {
    pub argv: Vec<OsString>,
    pub cwd_subject_ref: OpaquePermissionSubjectRef,
    pub structured_command_digest: CanonicalHash,
    pub admission_ref: crate::resource::OpaqueAdmissionId,
    pub execution_plan_draft_hash: CanonicalHash,
    pub environment_profile: EnvironmentProfileRefV1,
    pub capture: ExecutionCapturePolicy,
    pub limits: ExecutionResourceLimits,
    /// Initial PTY dimensions, when the approved capture contract requires a PTY.
    ///
    /// Dimensions are a bounded control input rather than host path material. Subsequent
    /// resizes still travel through `ManagedProcessHandleV1` and produce a typed control receipt.
    pub pty_size: Option<BoundedPtySizeV1>,
    /// Agreed environment (config-granted extension/server env); must match the sealed plan.
    pub environment: Vec<(OsString, OsString)>,
}

/// Output channel classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManagedProcessOutputChannelV1 {
    Stdout,
    Stderr,
    Pty,
}

/// Bounded output frame; exactly one terminal EOF per process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedProcessOutputFrameV1 {
    pub channel: ManagedProcessOutputChannelV1,
    pub sequence: u64,
    pub payload: Vec<u8>,
    pub end_of_stream: bool,
    pub truncated: bool,
}

/// Bounded process input for stdin writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedProcessInputV1 {
    pub payload: Vec<u8>,
}

/// Bounded PTY size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedPtySizeV1 {
    pub rows: u16,
    pub cols: u16,
}

/// Cancel reason classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessCancelReasonV1 {
    UserCancelled,
    DeadlineExceeded,
    ParentShutdown,
    QuotaExceeded,
}

/// Closed process termination classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessTerminationV1 {
    NotSpawned,
    Exited { code: i32 },
    Signaled { signal: u32 },
    Cancelled,
    TimedOut,
    OutcomeUncertain { evidence_digest: CanonicalHash },
}

/// Bounded output summary (never fabricated by a consumer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedOutputSummaryV1 {
    pub observed_bytes: u64,
    pub retained_bytes: u64,
    /// Bounded bytes retained by the authority-owned capture policy. This is the only output
    /// payload that a consumer may project; consumers must not re-open a process path or rebuild
    /// output from an unbounded post-execution source.
    pub retained_payload: Vec<u8>,
    pub content_digest: CanonicalHash,
    pub truncated: bool,
    pub artifact_ref: Option<crate::resource::OpaqueArtifactRefV1>,
}

/// Resource usage snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceUsageSnapshotV1 {
    pub logical_bytes: u64,
    pub physical_bytes: u64,
    pub entry_count: u64,
    pub open_holder_count: u64,
}

/// Access-widening policy requested by the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccessWideningPolicyV1 {
    Exact,
    AllowDeclaredSuperset { declaration_hash: CanonicalHash },
    ExplicitUnconfined,
}

/// Access-widening receipt (observed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessWideningReceiptV1 {
    pub requested: BTreeSet<ResourceAccessV1>,
    pub effective: BTreeSet<ResourceAccessV1>,
    pub unavoidable_widening: BTreeSet<ResourceAccessV1>,
    pub proof_digest: CanonicalHash,
}

/// Policy satisfaction classification (observed, never requested self-report).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccessPolicySatisfactionV1 {
    Exact,
    DeclaredSuperset { declaration_hash: CanonicalHash },
    ExplicitUnconfined,
}

/// Per-resource enforcement receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceEnforcementReceiptV1 {
    pub resource_ref: ResourceRefV1,
    pub access: AccessWideningReceiptV1,
    pub requested_policy: AccessWideningPolicyV1,
    pub policy_satisfaction: AccessPolicySatisfactionV1,
    pub enforcement: EnforcementCompletenessV1,
    pub proof_digest: CanonicalHash,
}

/// Execution-level resource receipt (requested vs effective, per resource).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionResourceReceiptV1 {
    pub physical_attempt_id: PhysicalAttemptId,
    pub manifest_hash: CanonicalHash,
    pub sandbox_binding_hash: CanonicalHash,
    pub requested_enforcement: RequestedEnforcementV1,
    pub effective_enforcement: EffectiveEnforcementV1,
    pub resources: Vec<ResourceEnforcementReceiptV1>,
    pub enforcement_proof_set_hash: CanonicalHash,
    pub environment_binding_hash: CanonicalHash,
    pub cleanup_status: ResourceCleanupStatusV1,
    pub effect_settlement: EffectSettlementV1,
}

/// Process control action classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessControlActionV1 {
    WriteStdin,
    ResizePty,
    Cancel,
    Terminate,
}

/// Typed process control receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessControlReceiptV1 {
    pub process_ref: ReflectiveOpaqueProcessRef,
    pub action: ProcessControlActionV1,
    pub request_digest: CanonicalHash,
    pub observed_process_frontier_hash: CanonicalHash,
    pub effect_settlement: EffectSettlementV1,
    pub receipt_hash: CanonicalHash,
}

/// Process execution receipt (RA constructed; cannot be synthesized by a caller).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessExecutionReceiptV1 {
    pub physical_attempt_id: PhysicalAttemptId,
    pub spawn_intent_id: crate::resource::OpaqueSpawnIntentId,
    pub process_ref: Option<ReflectiveOpaqueProcessRef>,
    pub process_frontier_hash: CanonicalHash,
    pub termination: ProcessTerminationV1,
    pub stdout_summary: BoundedOutputSummaryV1,
    pub stderr_summary: BoundedOutputSummaryV1,
    pub effect_settlement: EffectSettlementV1,
    pub receipt_hash: CanonicalHash,
}

/// Borrowed-resource access receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BorrowedResourceAccessReceiptV1 {
    pub subject_ref: OpaquePermissionSubjectRef,
    pub subject_binding_hash: CanonicalHash,
    pub operation_digest: CanonicalHash,
    pub granted_access_hash: CanonicalHash,
    pub identity_before: Option<CanonicalHash>,
    pub identity_after: Option<CanonicalHash>,
    pub borrowed_effect_frontier_hash: CanonicalHash,
    pub effect_settlement: EffectSettlementV1,
    pub receipt_hash: CanonicalHash,
}

/// Whether the execution contract observed enough of a pipeline to support repository
/// verification. A final-stage exit code is intentionally not equivalent to full evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineOutcomeV1 {
    NotPipeline,
    AllStagesObserved {
        stage_statuses_digest: CanonicalHash,
    },
    FinalStageOnly {
        final_exit_code: i32,
    },
}

/// Why a check receipt cannot be promoted to verification evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationEvidenceReasonV1 {
    UpstreamStatusUnobserved,
    ShellProfileDoesNotSupportPipefail,
}

/// Evidence sufficiency is kernel-owned; product surfaces only project this result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationEvidenceV1 {
    Sufficient,
    Insufficient {
        reason: VerificationEvidenceReasonV1,
    },
}

/// Execution check receipt (verification pipeline outcome reference).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCheckReceiptV1 {
    pub check_spec_hash: CanonicalHash,
    pub pipeline_outcome: PipelineOutcomeV1,
    pub verification_evidence: VerificationEvidenceV1,
    pub evidence_binding_hash: CanonicalHash,
    pub shell_profile_hash: CanonicalHash,
}

impl ExecutionCheckReceiptV1 {
    /// Final-stage-only execution can be a truthful process result, but never a passed
    /// repository verification result.
    #[must_use]
    pub fn verification_passed(&self) -> bool {
        !matches!(
            self.pipeline_outcome,
            PipelineOutcomeV1::FinalStageOnly { .. }
        ) && matches!(
            self.verification_evidence,
            VerificationEvidenceV1::Sufficient
        )
    }
}

/// Combined managed execution receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedExecutionReceiptV1 {
    pub physical_attempt_id: PhysicalAttemptId,
    pub process: ProcessExecutionReceiptV1,
    pub resources: ExecutionResourceReceiptV1,
    pub check: Option<ExecutionCheckReceiptV1>,
}

/// Async trait for live output streams (drained by the supervisor, bounded fan-out).
#[async_trait::async_trait]
pub trait ManagedProcessOutputStreamV1: Send {
    async fn next_frame(
        &mut self,
    ) -> Result<Option<BoundedProcessOutputFrameV1>, ManagedProcessControlErrorV1>;
}

/// Async trait for persistent process handles (non-clone, non-serialize).
#[async_trait::async_trait]
pub trait ManagedProcessHandleV1: Send {
    fn process_ref(&self) -> ReflectiveOpaqueProcessRef;
    fn physical_attempt_id(&self) -> PhysicalAttemptId;
    fn take_output_stream(
        &mut self,
    ) -> Result<Box<dyn ManagedProcessOutputStreamV1>, ManagedProcessControlErrorV1>;
    async fn write_stdin(
        &mut self,
        input: BoundedProcessInputV1,
    ) -> Result<ProcessControlReceiptV1, ManagedProcessControlErrorV1>;
    async fn resize_pty(
        &mut self,
        size: BoundedPtySizeV1,
    ) -> Result<ProcessControlReceiptV1, ManagedProcessControlErrorV1>;
    async fn close_stdin(
        &mut self,
    ) -> Result<ProcessControlReceiptV1, ManagedProcessControlErrorV1>;
    async fn cancel(
        &mut self,
        reason: ProcessCancelReasonV1,
    ) -> Result<ProcessControlReceiptV1, ManagedProcessControlErrorV1>;
    async fn wait_and_finalize(
        self: Box<Self>,
    ) -> Result<ManagedExecutionReceiptV1, ManagedExecutionErrorV1>;
}

/// Consumer-facing managed execution service.
#[async_trait::async_trait]
pub trait ManagedExecutionServiceV1: Send + Sync {
    async fn execute_once(
        &self,
        bundle: crate::resource::IssuedExecutionAdmissionBundleV1,
        request: ManagedExecutionRequestV1,
    ) -> Result<ManagedExecutionReceiptV1, ManagedExecutionErrorV1>;

    async fn start_persistent(
        &self,
        bundle: crate::resource::IssuedExecutionAdmissionBundleV1,
        request: ManagedExecutionRequestV1,
    ) -> Result<Box<dyn ManagedProcessHandleV1>, ManagedExecutionErrorV1>;
}

/// Closed execution error taxonomy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManagedExecutionErrorV1 {
    #[error("execution admission bundle does not match the approved request")]
    AdmissionMismatch,
    #[error("argv or structured command drifted after approval; replan required")]
    ExecutionPlanDrift,
    #[error("sandbox provider is unavailable; refusing implicit Local fallback")]
    ProviderUnavailable,
    #[error("process tree ownership is unavailable: {0}")]
    ProcessOwnershipUnavailable(String),
    #[error("required confinement not proven")]
    ConfinementUnproven,
    #[error("process settlement is outcome-uncertain")]
    OutcomeUncertain,
}

/// Closed control error taxonomy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManagedProcessControlErrorV1 {
    #[error("process output stream was already taken")]
    StreamAlreadyTaken,
    #[error("process is not in a writable state for {action}")]
    InvalidState { action: &'static str },
    #[error("process handle is stale after supervisor restart")]
    StaleHandle,
}

#[cfg(test)]
#[path = "tests/managed_execution_tests.rs"]
mod tests;
