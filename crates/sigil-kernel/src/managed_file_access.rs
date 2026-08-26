//! RFC-0071 section 8.5: in-process managed file access port.
//!
//! read/write/edit/list/glob/grep tools never spawn, but they must not bypass the borrowed
//! Workspace / ExternalUserPath identity lease. This module is the kernel pathless post-decision
//! port; local descriptors, leases and identities live in the authority implementation.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::recovery::EffectSettlementV1;
use crate::resource::{
    AuthorityGeneration, CanonicalHash, OpaqueBlockerId, OpaqueDomainEventId,
    OpaqueKernelCapabilityAuthenticatorV1, OpaqueKernelCapabilityHandleId,
    OpaquePermissionSubjectRef, OpaqueResolutionAttemptId, OpaqueSelectionNonce,
    OpaqueSessionExportId, ResourceAccessV1,
};

/// Pathless logical input accepted by the managed-file planner. The string is a workspace
/// relative selector; a host/authority implementation resolves it only after registration. It
/// is deliberately not a `PathBuf` and must never contain a host absolute path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedFileLogicalPathV1(pub String);

impl ManagedFileLogicalPathV1 {
    pub fn new(value: impl Into<String>) -> Result<Self, ManagedFileAccessErrorV1> {
        let value = value.into();
        if value.is_empty()
            || value.starts_with('/')
            || value.starts_with('\\')
            || value.contains('\0')
            || value.split('/').any(|part| part == "..")
            || value.contains(':')
        {
            return Err(ManagedFileAccessErrorV1::AliasCollision);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Side-effect-free planning request. The authority returns a pathless V3 plan reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedFileAccessPlanRequestV1 {
    pub logical_path: ManagedFileLogicalPathV1,
    pub operation: ManagedFileOperationV1,
    pub operation_scope: String,
}

/// Bounded physical operation input. Physical path resolution remains authority-private.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManagedFileExecutionInputV1 {
    Read {
        offset: usize,
        limit: usize,
        max_bytes: usize,
    },
    List {
        recursive: bool,
        limit: usize,
        max_depth: usize,
    },
    Glob {
        pattern: String,
        limit: usize,
    },
    Grep {
        pattern: String,
        limit: usize,
        max_bytes: usize,
    },
    Write {
        content: String,
    },
    Edit {
        old_text: String,
        new_text: String,
    },
    Delete,
}

/// Authority-private executor request after kernel seal/issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedFileExecutionRequestV1 {
    pub access: ManagedFileAccessRequestV1,
    pub input: ManagedFileExecutionInputV1,
}

/// Bounded result returned by the authority executor; callers do not perform a second filesystem
/// read. `payload` is already policy-safe text/JSON and is bounded by the authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedFileExecutionOutcomeV1 {
    pub access_receipt: crate::managed_execution::BorrowedResourceAccessReceiptV1,
    pub effect_settlement: EffectSettlementV1,
    pub result_digest: CanonicalHash,
    pub payload: String,
    pub observed_bytes: u64,
    pub returned_entries: u64,
    pub total_entries: u64,
    pub returned_lines: u64,
    pub total_lines: u64,
    pub truncated: bool,
}

/// Authority-owned pre-approval preview input. A preview may inspect only the exact pathless
/// plan it was given; it never receives a physical path and it never grants mutation authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedFilePreviewRequestV1 {
    pub plan_hash: CanonicalHash,
    pub operation: ManagedFileOperationV1,
    pub max_bytes: usize,
}

/// Bounded result for a physical preview read performed by the authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedFilePreviewOutcomeV1 {
    pub payload: String,
    pub observed_bytes: u64,
    pub result_digest: CanonicalHash,
    pub truncated: bool,
}

/// Closed in-process file operation classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManagedFileOperationV1 {
    Read,
    List,
    Glob,
    Grep,
    Write,
    Edit,
    Delete,
    Rename,
}

/// Post-decision file access request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedFileAccessRequestV1 {
    pub subject_ref: OpaquePermissionSubjectRef,
    pub operation: ManagedFileOperationV1,
    pub operation_digest: CanonicalHash,
    pub admission_binding: ManagedFileAdmissionBindingV1,
    pub admission_binding_hash: CanonicalHash,
}

/// RFC-0002 workspace mutation activation binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceMutationFileActivationBindingV1 {
    pub admission_bundle_hash: CanonicalHash,
    pub lease_holder_id: String,
    pub lease_acquisition_proof_hash: CanonicalHash,
    pub acquired_lease_epoch: u64,
    pub snapshot_preparation_receipt_hash: CanonicalHash,
    pub mutation_prepared_event_hash: CanonicalHash,
    pub mutation_prepared_frontier_hash: CanonicalHash,
    pub activation_hash: CanonicalHash,
}

/// Closed file admission binding sources.
/// Variants are deliberately flat (fixed closed schema); size differences are an accepted
/// contract tradeoff, never a hint that SessionExport facts are optional.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManagedFileAdmissionBindingV1 {
    ToolPermissionPlan {
        permission_plan_hash: CanonicalHash,
        decision_hash: CanonicalHash,
        approval_continuity_hash: CanonicalHash,
        tool_start_event_digest: CanonicalHash,
        file_access_plan_hash: CanonicalHash,
        file_subject_binding_hash: CanonicalHash,
        file_resolver_proof_digest: CanonicalHash,
        file_authority_generation: AuthorityGeneration,
        workspace_mutation_activation: Option<WorkspaceMutationFileActivationBindingV1>,
    },
    SessionExport {
        admission_hash: CanonicalHash,
        export_planned_event_hash: CanonicalHash,
        create_intent_hash: CanonicalHash,
        create_prepared_event_hash: CanonicalHash,
        create_initiated_event_hash: CanonicalHash,
        recovery_subject_bound_event_hash: Option<CanonicalHash>,
        activation_frontier_hash: CanonicalHash,
    },
    SessionExportReconcile {
        recovery_admission_hash: CanonicalHash,
        export_planned_event_hash: CanonicalHash,
        expected_content_digest: CanonicalHash,
        recovery_started_event_hash: CanonicalHash,
        recovery_subject_bound_event_hash: CanonicalHash,
        recovery_started_frontier_hash: CanonicalHash,
    },
}

/// Closed file access admission token envelope.
#[derive(Debug)]
pub enum ManagedFileAccessAdmissionTokenV1 {
    Tool(ToolFileAccessAdmissionTokenV1),
    SessionExport(SessionExportFileAdmissionTokenV1),
    SessionExportReconcile(SessionExportReconcileAdmissionTokenV1),
}

/// Tool-bound file access token (one claim).
#[derive(Debug)]
pub struct ToolFileAccessAdmissionTokenV1 {
    binding: ManagedFileAdmissionBindingV1,
    subject_binding_hash: CanonicalHash,
    operation_digest: CanonicalHash,
    #[allow(dead_code)]
    claim: NonCloneOneShotClaim,
}

impl ToolFileAccessAdmissionTokenV1 {
    /// Kernel-broker-issued tool token (real admission; binding/op/subject are kernel-bound
    /// at seal time, never fabricated by a consumer).
    pub(crate) fn broker_issued(
        binding: ManagedFileAdmissionBindingV1,
        subject_binding_hash: CanonicalHash,
        operation_digest: CanonicalHash,
    ) -> Self {
        Self {
            binding,
            subject_binding_hash,
            operation_digest,
            claim: NonCloneOneShotClaim {
                _authenticator: OpaqueKernelCapabilityAuthenticatorV1::new(
                    "broker-file-access".to_owned(),
                ),
            },
        }
    }

    /// Kernel-owned qualification fixture handle (R71.6 conformance only). Production
    /// issuance is the capability broker inside kernel; this constructor exists so the
    /// authority adjudicator conformance fixtures can build an exact token shape.
    pub fn qualification_fixture(
        binding: ManagedFileAdmissionBindingV1,
        subject_binding_hash: CanonicalHash,
        operation_digest: CanonicalHash,
    ) -> Self {
        Self {
            binding,
            subject_binding_hash,
            operation_digest,
            claim: NonCloneOneShotClaim {
                _authenticator: OpaqueKernelCapabilityAuthenticatorV1::new(
                    "qualification".to_owned(),
                ),
            },
        }
    }

    pub fn binding(&self) -> &ManagedFileAdmissionBindingV1 {
        &self.binding
    }

    pub fn subject_binding_hash(&self) -> CanonicalHash {
        self.subject_binding_hash
    }

    pub fn operation_digest(&self) -> CanonicalHash {
        self.operation_digest
    }
}

/// Session export external-selection intent (user confirmation-bound).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionExportExternalSelectionIntentV1 {
    pub export_id: OpaqueSessionExportId,
    pub selection_nonce: OpaqueSelectionNonce,
    pub requested_access: BTreeSet<ResourceAccessV1>,
    pub user_confirmation_hash: CanonicalHash,
    pub intent_hash: CanonicalHash,
}

/// Session export create intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionExportCreateIntentV1 {
    pub export_id: OpaqueSessionExportId,
    pub export_planned_event_hash: CanonicalHash,
    pub selection_intent_hash: CanonicalHash,
    pub destination_subject_ref: OpaquePermissionSubjectRef,
    pub destination_binding_hash: CanonicalHash,
    pub registration_receipt_hash: CanonicalHash,
    pub expected_parent_identity: CanonicalHash,
    pub leaf_name_digest: CanonicalHash,
    pub create_new_operation_digest: CanonicalHash,
    pub content_digest: CanonicalHash,
    pub byte_length: u64,
    pub user_confirmation_hash: CanonicalHash,
    pub intent_hash: CanonicalHash,
}

/// Session export file admission (durable facts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionExportFileAdmissionV1 {
    pub export_id: OpaqueSessionExportId,
    pub export_planned_event_id: OpaqueDomainEventId,
    pub export_planned_event_hash: CanonicalHash,
    pub session_source_frontier_hash: CanonicalHash,
    pub create_intent_hash: CanonicalHash,
    pub destination_subject_ref: OpaquePermissionSubjectRef,
    pub destination_binding_hash: CanonicalHash,
    pub create_new_operation_digest: CanonicalHash,
    pub user_confirmation_hash: CanonicalHash,
    pub create_prepared_event_id: OpaqueDomainEventId,
    pub create_prepared_event_hash: CanonicalHash,
    pub create_initiated_event_id: OpaqueDomainEventId,
    pub create_initiated_event_hash: CanonicalHash,
    pub activation_frontier_hash: CanonicalHash,
    pub recovery_started_event_hash: Option<CanonicalHash>,
    pub recovery_subject_bound_event_hash: Option<CanonicalHash>,
    pub admission_hash: CanonicalHash,
}

/// Session export file token (one claim).
#[derive(Debug)]
pub struct SessionExportFileAdmissionTokenV1 {
    admission: SessionExportFileAdmissionV1,
    #[allow(dead_code)]
    claim: NonCloneOneShotClaim,
}

impl SessionExportFileAdmissionTokenV1 {
    pub fn admission(&self) -> &SessionExportFileAdmissionV1 {
        &self.admission
    }
}

/// Session export reconcile admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionExportReconcileAdmissionV1 {
    pub export_id: OpaqueSessionExportId,
    pub export_planned_event_id: OpaqueDomainEventId,
    pub export_planned_event_hash: CanonicalHash,
    pub destination_subject_ref: OpaquePermissionSubjectRef,
    pub destination_binding_hash: CanonicalHash,
    pub expected_content_digest: CanonicalHash,
    pub expected_byte_length: u64,
    pub expected_destination_identity: CanonicalHash,
    pub blocker_id: OpaqueBlockerId,
    pub resolution_attempt_id: OpaqueResolutionAttemptId,
    pub recovery_action_token_hash: CanonicalHash,
    pub recovery_operation: SessionExportRecoveryOperationV1,
}

/// Closed session-export recovery operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionExportRecoveryOperationV1 {
    ReconcileCreateNew,
    ReselectDestination,
    SupersedeUncertain,
}

/// Reconcile token (one claim).
#[derive(Debug)]
pub struct SessionExportReconcileAdmissionTokenV1 {
    admission: SessionExportReconcileAdmissionV1,
    #[allow(dead_code)]
    claim: NonCloneOneShotClaim,
}

impl SessionExportReconcileAdmissionTokenV1 {
    pub fn admission(&self) -> &SessionExportReconcileAdmissionV1 {
        &self.admission
    }
}

/// File access outcome: bounded result plus borrowed mutation receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedFileAccessResultV1 {
    pub access_receipt: crate::managed_execution::BorrowedResourceAccessReceiptV1,
    pub effect_settlement: EffectSettlementV1,
    pub result_digest: CanonicalHash,
}

/// Object-safe post-decision file access service (authority implementation).
pub trait ManagedFileAccessServiceV1: Send + Sync {
    /// Creates a pathless plan from an already registered borrowed subject.
    fn plan(
        &self,
        _request: ManagedFileAccessPlanRequestV1,
    ) -> Result<crate::permission_plan_v3::ManagedFileAccessPlanDraftRefV1, ManagedFileAccessErrorV1>
    {
        Err(ManagedFileAccessErrorV1::ResourcePreconditionUnavailable)
    }

    fn access(
        &self,
        request: ManagedFileAccessRequestV1,
        token: ManagedFileAccessAdmissionTokenV1,
    ) -> Result<ManagedFileAccessResultV1, ManagedFileAccessErrorV1>;

    /// Executes one admitted operation. Implementations must perform physical I/O only behind
    /// this authority-owned method and return a bounded result.
    fn execute(
        &self,
        _request: ManagedFileExecutionRequestV1,
        _token: ManagedFileAccessAdmissionTokenV1,
    ) -> Result<ManagedFileExecutionOutcomeV1, ManagedFileAccessErrorV1> {
        Err(ManagedFileAccessErrorV1::ResourcePreconditionUnavailable)
    }

    /// Reads bounded preview data behind the authority-owned plan table without creating an
    /// execution admission. Builtin previews must use this port instead of touching the host FS.
    fn preview(
        &self,
        _request: ManagedFilePreviewRequestV1,
    ) -> Result<ManagedFilePreviewOutcomeV1, ManagedFileAccessErrorV1> {
        Err(ManagedFileAccessErrorV1::ResourcePreconditionUnavailable)
    }
}

/// Closed file access error taxonomy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManagedFileAccessErrorV1 {
    #[error("admission binding does not match the request")]
    AdmissionMismatch,
    #[error("subject identity drifted after approval")]
    SubjectIdentityDrift,
    #[error("separator, traversal, unicode case-fold or drive/UNC alias collision")]
    AliasCollision,
    #[error("operation not permitted for this binding")]
    OperationNotPermitted,
    #[error("managed file resource is not ready or workspace registration is missing")]
    ResourcePreconditionUnavailable,
    #[error("managed file admission token has already been consumed")]
    TokenReplay,
    #[error("managed file plan is stale or belongs to another authority generation")]
    PlanStale,
    #[error("managed file physical execution failed: {0}")]
    PhysicalExecutionFailed(String),
}

/// Non-clone one-shot claim marker.
#[derive(Debug)]
pub struct NonCloneOneShotClaim {
    _authenticator: OpaqueKernelCapabilityAuthenticatorV1,
}

/// Capability authenticator primary for internal brokers.
#[derive(Debug)]
pub struct KernelCapabilityPrivateHandle {
    #[allow(dead_code)]
    handle: OpaqueKernelCapabilityHandleId,
    #[allow(dead_code)]
    authenticator: OpaqueKernelCapabilityAuthenticatorV1,
}
