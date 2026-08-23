//! RFC-0071 section 8.2 / R71.6: V3 permission plan/decision producer.
//!
//! The V3 plan is the only sealed shape the cutover runtime consumes. This builder computes
//! the canonical plan hash (stable field order, no host path separators) and the decision
//! digest from the approval-bound fields; consumers never re-canonicalize or fabricate a
//! plan/decision from DTO text. It carries no I/O and never interprets a path.

use crate::permission_plan_v3::{
    ManagedExecutionPlanDraftRefV1, ManagedFileAccessPlanDraftRefV1, ManagedStoragePlanRefV1,
    PermissionConfirmationV3, ToolPermissionDecisionV3, ToolPermissionPlanCoreV3,
    ToolPermissionPlanV3, ToolPermissionPolicyFacetsV3,
};
use crate::resource::{
    CanonicalHash, RequestedEnforcementV1, ResourceJournalScopeV1, ResourceRequirementSetV1,
};

use sha2::Digest;

pub fn build_v3_plan(
    core: ToolPermissionPlanCoreV3,
    resource_requirements: ResourceRequirementSetV1,
    execution_plan_drafts: Vec<ManagedExecutionPlanDraftRefV1>,
    managed_storage_plans: Vec<ManagedStoragePlanRefV1>,
    managed_file_access_plan: Option<ManagedFileAccessPlanDraftRefV1>,
    attempt_journal_scope: ResourceJournalScopeV1,
    requested_enforcement: RequestedEnforcementV1,
) -> ToolPermissionPlanV3 {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"tool-permission-plan-v3");
    hasher.update(core.tool_name.as_bytes());
    hasher.update(format!("{:?}", core.access).as_bytes());
    hasher.update(format!("{:?}", core.operation).as_bytes());
    hasher.update(resource_requirements.canonical_hash.as_bytes());
    hasher.update(format!("{:?}", core.effects).as_bytes());
    for ref_draft in &execution_plan_drafts {
        hasher.update(ref_draft.draft_hash.as_bytes());
    }
    for storage in &managed_storage_plans {
        hasher.update(storage.plan_hash.as_bytes());
    }
    if let Some(file_ref) = &managed_file_access_plan {
        hasher.update(file_ref.plan_hash.as_bytes());
        hasher.update(file_ref.subject_binding_hash.as_bytes());
    }
    hasher.update(attempt_journal_scope_hash_bytes(&attempt_journal_scope));
    let plan_hash = CanonicalHash::from_bytes(hasher.finalize().into());
    ToolPermissionPlanV3 {
        core,
        resource_requirements,
        execution_plan_drafts,
        managed_storage_plans,
        managed_file_access_plan,
        attempt_journal_scope,
        attempt_journal_scope_hash: plan_hash,
        requested_enforcement,
        plan_hash,
    }
}

pub fn build_v3_decision(
    plan: &ToolPermissionPlanV3,
    decision_id: crate::resource::OpaquePermissionDecisionId,
    approval_request_id: crate::resource::OpaqueApprovalRequestId,
    approval_request_hash: CanonicalHash,
    call_id: crate::resource::OpaqueToolCallId,
    subject_binding_hash: CanonicalHash,
    policy_version: &str,
    policy_decision: crate::permission::ApprovalMode,
    policy_facets: ToolPermissionPolicyFacetsV3,
    confirmation: Option<PermissionConfirmationV3>,
    grant_ref: Option<crate::resource::OpaqueSessionGrantRef>,
    prepared_intent_digest: Option<CanonicalHash>,
) -> ToolPermissionDecisionV3 {
    let execution_draft_hashes: Vec<CanonicalHash> = plan
        .execution_plan_drafts
        .iter()
        .map(|ref_draft| ref_draft.draft_hash)
        .collect();
    let managed_storage_plan_hashes: Vec<CanonicalHash> = plan
        .managed_storage_plans
        .iter()
        .map(|storage| storage.plan_hash)
        .collect();
    let confirmation_hash = confirmation
        .as_ref()
        .map(|confirm| confirm.accepted_hash)
        .unwrap_or(CanonicalHash::from_bytes([0u8; 32]));
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"tool-permission-decision-v3");
    hasher.update(plan.plan_hash.as_bytes());
    hasher.update(subject_binding_hash.as_bytes());
    hasher.update(policy_version.as_bytes());
    hasher.update(confirmation_hash.as_bytes());
    let decision_hash = CanonicalHash::from_bytes(hasher.finalize().into());
    ToolPermissionDecisionV3 {
        schema_version: 1,
        decision_id,
        approval_request_id,
        approval_request_hash,
        call_id,
        tool_name: plan.core.tool_name.clone(),
        plan_hash: plan.plan_hash,
        requirement_set_hash: plan.resource_requirements.canonical_hash,
        execution_draft_hashes,
        managed_storage_plan_hashes,
        managed_file_access_plan_hash: plan
            .managed_file_access_plan
            .as_ref()
            .map(|file_ref| file_ref.plan_hash),
        attempt_journal_scope_hash: plan.attempt_journal_scope_hash,
        subject_binding_hash,
        requested_enforcement_hash: plan.requested_enforcement.profile_hash,
        policy_version: policy_version.to_owned(),
        policy_decision,
        policy_facets,
        confirmation,
        grant_ref,
        prepared_intent_digest,
        decision_hash,
    }
}

fn attempt_journal_scope_hash_bytes(scope: &ResourceJournalScopeV1) -> &'static [u8] {
    match scope {
        ResourceJournalScopeV1::Application => b"application",
        ResourceJournalScopeV1::Workspace(_) => b"workspace",
    }
}

#[cfg(test)]
#[path = "tests/permission_plan_v3_builder_tests.rs"]
mod tests;
