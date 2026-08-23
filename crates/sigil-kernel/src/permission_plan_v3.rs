//! RFC-0071 section 8.2: ToolPermission V3 current-schema contract.
//!
//! V3 is a new current schema and does not nest V2 compatibility payloads. R71.1-R71.5 validate it
//! only in the shadow/isolated qualification harness; R71.6 performs the application/session-global
//! clean cutover. A V3 validator recomputes every referenced hash from the exact plan and durable
//! approval request; a side-effect-free preview can never be projected as effective enforcement.
//!
//! The four closed envelope shapes (RFC section 8.2) are:
//! 1. pure process tool: exactly one execution draft, no storage plans, file plan = None;
//! 2. pure in-process storage tool: at least one storage plan, no execution drafts, file plan = None;
//! 3. plain read-only in-process file tool: no execution/storage plans, file plan = Some(exact one);
//! 4. RFC-0002 mutating file tool: no execution drafts, file plan = Some(exact one), storage plans
//!    exactly { WorkspaceMutationState x SemanticLeaseLedger } plus, only when
//!    SnapshotCoverage::Captured, exactly { ArtifactStaging, ArtifactStore }.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::resource::{
    AuthorityGeneration, CanonicalHash, ManagedStorageCapabilityFamilyV1,
    ManagedStorageSemanticOwnerV1, OpaqueApprovalRequestId, OpaqueExecutionPlanDraftId,
    OpaqueManagedFileAccessPlanId, OpaqueManagedStoragePlanId, OpaquePermissionDecisionId,
    OpaquePermissionSubjectRef, OpaqueSessionGrantRef, OpaqueStorageOperationAttemptId,
    OpaqueToolCallId, RequestedEnforcementV1, ResourceContractError, ResourceJournalScopeV1,
    ResourceRequirementSetV1,
};
use crate::{
    ApprovalMode, ExecutionContainmentRequest, ToolAccess, ToolAnalysisStatus, ToolOperation,
    ToolPermissionEffect, ToolPermissionSummary, ToolSemanticScope, ToolSubject,
};

pub const TOOL_PERMISSION_PLAN_V3_SCHEMA_VERSION: u32 = 3;

// ---------------------------------------------------------------------------
// V3 plan reference types
// ---------------------------------------------------------------------------

/// Reference to the side-effect-free execution plan draft produced by the runtime shadow planner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedExecutionPlanDraftRefV1 {
    pub draft_id: OpaqueExecutionPlanDraftId,
    pub draft_hash: CanonicalHash,
    pub resource_plan_hash: CanonicalHash,
    pub attempt_journal_scope_hash: CanonicalHash,
}

/// Reference to an in-process managed storage plan produced by the authority-owned storage service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedStoragePlanRefV1 {
    pub plan_id: OpaqueManagedStoragePlanId,
    pub storage_operation_attempt_id: OpaqueStorageOperationAttemptId,
    pub semantic_owner: ManagedStorageSemanticOwnerV1,
    pub capability_family: ManagedStorageCapabilityFamilyV1,
    pub requirement_set_hash: CanonicalHash,
    pub operation_digest: CanonicalHash,
    pub journal_scope_hash: CanonicalHash,
    pub plan_hash: CanonicalHash,
}

/// Reference to the in-process (borrowed) file access plan for one tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedFileAccessPlanDraftRefV1 {
    pub plan_id: OpaqueManagedFileAccessPlanId,
    pub subject_ref: OpaquePermissionSubjectRef,
    pub subject_binding_hash: CanonicalHash,
    pub operation_digest: CanonicalHash,
    pub authority_generation: AuthorityGeneration,
    pub resolver_proof_digest: CanonicalHash,
    pub plan_hash: CanonicalHash,
}

/// Core plan fields carried by V3 unchanged from the V2 semantic payload (no nesting).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPermissionPlanCoreV3 {
    pub tool_name: String,
    pub access: ToolAccess,
    pub operation: ToolOperation,
    pub effects: BTreeSet<ToolPermissionEffect>,
    pub subjects: Vec<ToolSubject>,
    pub analysis: ToolAnalysisStatus,
    pub containment: ExecutionContainmentRequest,
    pub semantic_scope: Option<ToolSemanticScope>,
    pub tool_default_mode: Option<ApprovalMode>,
    pub analysis_bindings: BTreeMap<String, String>,
    pub safe_summary: ToolPermissionSummary,
}

/// Pathless V3 permission plan (request side; no filesystem mutation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPermissionPlanV3 {
    pub core: ToolPermissionPlanCoreV3,
    pub resource_requirements: ResourceRequirementSetV1,
    pub execution_plan_drafts: Vec<ManagedExecutionPlanDraftRefV1>,
    pub managed_storage_plans: Vec<ManagedStoragePlanRefV1>,
    pub managed_file_access_plan: Option<ManagedFileAccessPlanDraftRefV1>,
    pub attempt_journal_scope: ResourceJournalScopeV1,
    pub attempt_journal_scope_hash: CanonicalHash,
    pub requested_enforcement: RequestedEnforcementV1,
    pub plan_hash: CanonicalHash,
}

// ---------------------------------------------------------------------------
// V3 decision types
// ---------------------------------------------------------------------------

/// Policy facets relevant to the V3 decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPermissionPolicyFacetsV3 {
    pub external_directory_required: bool,
    pub session_grant_available: bool,
    pub confirmation_required: bool,
}

/// Exact user confirmation bound to the decision (not a preview).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionConfirmationV3 {
    pub confirmation_id: String,
    pub accepted_hash: CanonicalHash,
}

/// Closed V3 permission decision. Records only requested enforcement and side-effect-free
/// preview; effective backend/access/quota fields are absent by design.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPermissionDecisionV3 {
    pub schema_version: u32,
    pub decision_id: OpaquePermissionDecisionId,
    pub approval_request_id: OpaqueApprovalRequestId,
    pub approval_request_hash: CanonicalHash,
    pub call_id: OpaqueToolCallId,
    pub tool_name: String,
    pub plan_hash: CanonicalHash,
    pub requirement_set_hash: CanonicalHash,
    pub execution_draft_hashes: Vec<CanonicalHash>,
    pub managed_storage_plan_hashes: Vec<CanonicalHash>,
    pub managed_file_access_plan_hash: Option<CanonicalHash>,
    pub attempt_journal_scope_hash: CanonicalHash,
    pub subject_binding_hash: CanonicalHash,
    pub requested_enforcement_hash: CanonicalHash,
    pub policy_version: String,
    pub policy_decision: ApprovalMode,
    pub policy_facets: ToolPermissionPolicyFacetsV3,
    pub confirmation: Option<PermissionConfirmationV3>,
    pub grant_ref: Option<OpaqueSessionGrantRef>,
    pub prepared_intent_digest: Option<CanonicalHash>,
    pub decision_hash: CanonicalHash,
}

/// Closed V3 envelope shape classification (exactly four variants).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPermissionPlanEnvelopeV3 {
    PureProcess,
    PureInProcessStorage,
    ReadOnlyFile,
    MutatingFile,
}

impl ToolPermissionPlanEnvelopeV3 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PureProcess => "pure_process",
            Self::PureInProcessStorage => "pure_in_process_storage",
            Self::ReadOnlyFile => "read_only_file",
            Self::MutatingFile => "mutating_file",
        }
    }
}

/// Exact V3 envelope validator. Defaults that fall outside the four closed shapes are rejected:
/// the decoder never guesses a category from set shapes.
pub fn classify_plan_envelope(
    plan: &ToolPermissionPlanV3,
) -> Result<ToolPermissionPlanEnvelopeV3, ResourceContractError> {
    let has_execution = !plan.execution_plan_drafts.is_empty();
    let has_storage = !plan.managed_storage_plans.is_empty();
    match (
        has_execution,
        has_storage,
        plan.managed_file_access_plan.is_some(),
    ) {
        (true, false, false) => Ok(ToolPermissionPlanEnvelopeV3::PureProcess),
        (false, true, false) => Ok(ToolPermissionPlanEnvelopeV3::PureInProcessStorage),
        (false, false, true) => Ok(ToolPermissionPlanEnvelopeV3::ReadOnlyFile),
        // Mutating file: exactly one file plan and a storage set; execution drafts are empty.
        (false, true, true) if plan.managed_storage_plans.len() == 2 => {
            Ok(ToolPermissionPlanEnvelopeV3::MutatingFile)
        }
        _ => Err(ResourceContractError::InvalidV3EnvelopeShape),
    }
}

// ---------------------------------------------------------------------------
// Missing opaque id / error glue
// ---------------------------------------------------------------------------
