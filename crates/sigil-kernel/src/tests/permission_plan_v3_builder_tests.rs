//! RFC-0071 R71.6: V3 plan/decision producer qualification.

use crate::permission::ApprovalMode;
use crate::permission_plan_v3::{
    ManagedFileAccessPlanDraftRefV1, PermissionConfirmationV3, ToolPermissionPlanCoreV3,
    ToolPermissionPolicyFacetsV3,
};
use crate::permission_plan_v3_builder::{build_v3_decision, build_v3_plan};
use crate::resource::{
    AuthorityGeneration, BoundedVec, CanonicalHash, EnforcementRequirementClassV1,
    OpaqueApprovalRequestId, OpaqueManagedFileAccessPlanId, OpaquePermissionDecisionId,
    OpaquePermissionSubjectRef, OpaqueToolCallId, RequestedEnforcementV1, ResourceJournalScopeV1,
    ResourceRequirementSetV1,
};

fn core() -> ToolPermissionPlanCoreV3 {
    ToolPermissionPlanCoreV3 {
        tool_name: "read_file".to_owned(),
        access: crate::tool::ToolAccess::Read,
        operation: crate::ToolOperation::Read,
        effects: Default::default(),
        subjects: vec![crate::ToolSubject {
            kind: crate::ToolSubjectKind::Path,
            original: ".".to_owned(),
            normalized: ".".to_owned(),
            canonical_path: None,
            scope: crate::ToolSubjectScope::Workspace,
            access: crate::ToolAccess::Read,
        }],
        analysis: crate::ToolAnalysisStatus::Complete,
        containment: crate::ExecutionContainmentRequest::default(),
        semantic_scope: None,
        tool_default_mode: None,
        analysis_bindings: Default::default(),
        safe_summary: crate::ToolPermissionSummary {
            title: "read file".to_owned(),
            detail: "".to_owned(),
            step_count: 0,
            workspace_code_steps: 0,
        },
    }
}

fn file_ref() -> ManagedFileAccessPlanDraftRefV1 {
    ManagedFileAccessPlanDraftRefV1 {
        plan_id: OpaqueManagedFileAccessPlanId::new("fa-1".to_owned()),
        subject_ref: OpaquePermissionSubjectRef::new("ws-1".to_owned()),
        subject_binding_hash: CanonicalHash::from_bytes([0x11; 32]),
        operation_digest: CanonicalHash::from_bytes([0x12; 32]),
        authority_generation: AuthorityGeneration {
            epoch: 1,
            instance_hash: CanonicalHash::from_bytes([0x13; 32]),
        },
        resolver_proof_digest: CanonicalHash::from_bytes([0x14; 32]),
        plan_hash: CanonicalHash::from_bytes([0x15; 32]),
    }
}

fn enforcement() -> RequestedEnforcementV1 {
    RequestedEnforcementV1 {
        requirement: EnforcementRequirementClassV1::ExplicitUnconfined,
        deny_ambient_system_temp_write: false,
        deny_ambient_home_write: false,
        deny_ungranted_workspace_write: false,
        require_process_tree_ownership: false,
        require_network_policy: false,
        requested_capability_set_hash: CanonicalHash::from_bytes([0x21; 32]),
        profile_hash: CanonicalHash::from_bytes([0x22; 32]),
    }
}

#[test]
fn r71_v3_plan_hash_is_deterministic_and_binds_file_ref() {
    let requirements = ResourceRequirementSetV1 {
        schema_version: 1,
        requirements: BoundedVec::new(),
        canonical_hash: CanonicalHash::from_bytes([0x31; 32]),
    };
    let plan_a = build_v3_plan(
        core(),
        requirements.clone(),
        Vec::new(),
        Vec::new(),
        Some(file_ref()),
        ResourceJournalScopeV1::Application,
        enforcement(),
    );
    let plan_b = build_v3_plan(
        core(),
        requirements,
        Vec::new(),
        Vec::new(),
        Some(file_ref()),
        ResourceJournalScopeV1::Application,
        enforcement(),
    );
    assert_eq!(plan_a.plan_hash, plan_b.plan_hash);
    assert_ne!(plan_a.plan_hash, CanonicalHash::from_bytes([0u8; 32]));
    // A plan without the file ref binds a different hash (hash covers the ref).
    let no_file = build_v3_plan(
        core(),
        ResourceRequirementSetV1 {
            schema_version: 1,
            requirements: BoundedVec::new(),
            canonical_hash: CanonicalHash::from_bytes([0x31; 32]),
        },
        Vec::new(),
        Vec::new(),
        None,
        ResourceJournalScopeV1::Application,
        enforcement(),
    );
    assert_ne!(plan_a.plan_hash, no_file.plan_hash);
}

#[test]
fn r71_v3_decision_digest_binds_plan_and_confirmation() {
    let requirements = ResourceRequirementSetV1 {
        schema_version: 1,
        requirements: BoundedVec::new(),
        canonical_hash: CanonicalHash::from_bytes([0x31; 32]),
    };
    let plan = build_v3_plan(
        core(),
        requirements,
        Vec::new(),
        Vec::new(),
        Some(file_ref()),
        ResourceJournalScopeV1::Application,
        enforcement(),
    );
    let confirmation = PermissionConfirmationV3 {
        confirmation_id: "confirm-1".to_owned(),
        accepted_hash: CanonicalHash::from_bytes([0x41; 32]),
    };
    let decision = build_v3_decision(
        &plan,
        OpaquePermissionDecisionId::new("decision-1".to_owned()),
        OpaqueApprovalRequestId::new("approval-1".to_owned()),
        CanonicalHash::from_bytes([0x42; 32]),
        OpaqueToolCallId::new("call-1".to_owned()),
        CanonicalHash::from_bytes([0x11; 32]),
        "permission-v3",
        ApprovalMode::Allow,
        ToolPermissionPolicyFacetsV3 {
            external_directory_required: false,
            session_grant_available: false,
            confirmation_required: false,
        },
        Some(confirmation),
        None,
        None,
    );
    assert_eq!(decision.plan_hash, plan.plan_hash);
    assert_ne!(decision.decision_hash, CanonicalHash::from_bytes([0u8; 32]));
    assert_eq!(
        decision.managed_file_access_plan_hash,
        Some(file_ref().plan_hash)
    );
}

#[test]
fn r71_v3_plan_from_approved_v2_is_sealed_deterministic() {
    use crate::permission_plan_v3_builder::v3_plan_from_v2;
    let v2 = crate::permission_plan::ToolPermissionPlanV2 {
        schema_version: 1,
        tool_name: "read_file".to_owned(),
        access: crate::ToolAccess::Read,
        operation: crate::ToolOperation::Read,
        effects: Default::default(),
        subjects: vec![crate::ToolSubject {
            kind: crate::ToolSubjectKind::Path,
            original: ".".to_owned(),
            normalized: ".".to_owned(),
            canonical_path: None,
            scope: crate::ToolSubjectScope::Workspace,
            access: crate::ToolAccess::Read,
        }],
        analysis: crate::ToolAnalysisStatus::Complete,
        containment: crate::ExecutionContainmentRequest::default(),
        semantic_scope: None,
        tool_default_mode: None,
        analysis_bindings: Default::default(),
        plan_hash: "v2-hash".to_owned(),
        safe_summary: crate::ToolPermissionSummary {
            title: "read".to_owned(),
            detail: "".to_owned(),
            step_count: 0,
            workspace_code_steps: 0,
        },
        managed_file_access: None,
    };
    let a = v3_plan_from_v2(&v2);
    let b = v3_plan_from_v2(&v2);
    assert_eq!(a.plan_hash, b.plan_hash);
    assert_ne!(a.plan_hash, CanonicalHash::from_bytes([0u8; 32]));
    // Unspecified containment stays ExplicitUnconfined (transitional V3 classes).
    assert_eq!(
        a.requested_enforcement.requirement,
        EnforcementRequirementClassV1::ExplicitUnconfined
    );
    // Sealing with a declared file ref changes the bound hash (the transform reads the ref
    // from the approved plan; the admission stays content-covering).
    let mut v2_with_ref = v2.clone();
    v2_with_ref.managed_file_access = Some(file_ref());
    let with_ref = v3_plan_from_v2(&v2_with_ref);
    assert_ne!(a.plan_hash, with_ref.plan_hash);
    assert_eq!(with_ref.managed_file_access_plan, Some(file_ref()));
}
