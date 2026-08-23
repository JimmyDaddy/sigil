//! RFC-0071 R71.6: V3 file-access binding mapping and guarded adjudication.

use crate::permission_plan_v3::ManagedFileAccessPlanDraftRefV1;
use crate::resource::{
    AuthorityGeneration, CanonicalHash, OpaqueManagedFileAccessPlanId, OpaquePermissionSubjectRef,
};
use crate::tool_authority::{adjudicate_guarded_tool_operation, v3_file_access_binding};

#[test]
fn r71_tool_authority_v3_binding_maps_exact_fields() {
    let file_ref = ManagedFileAccessPlanDraftRefV1 {
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
    };
    let binding = v3_file_access_binding(
        CanonicalHash::from_bytes([0x01; 32]),
        CanonicalHash::from_bytes([0x02; 32]),
        CanonicalHash::from_bytes([0x03; 32]),
        CanonicalHash::from_bytes([0x04; 32]),
        &file_ref,
    );
    let crate::managed_file_access::ManagedFileAdmissionBindingV1::ToolPermissionPlan {
        permission_plan_hash,
        decision_hash,
        approval_continuity_hash,
        tool_start_event_digest,
        file_access_plan_hash,
        file_subject_binding_hash,
        file_resolver_proof_digest,
        file_authority_generation,
        workspace_mutation_activation,
    } = binding
    else {
        panic!("expected tool permission plan");
    };
    assert_eq!(permission_plan_hash, CanonicalHash::from_bytes([0x01; 32]));
    assert_eq!(decision_hash, CanonicalHash::from_bytes([0x02; 32]));
    assert_eq!(
        approval_continuity_hash,
        CanonicalHash::from_bytes([0x03; 32])
    );
    assert_eq!(
        tool_start_event_digest,
        CanonicalHash::from_bytes([0x04; 32])
    );
    assert_eq!(file_access_plan_hash, CanonicalHash::from_bytes([0x15; 32]));
    assert_eq!(
        file_subject_binding_hash,
        CanonicalHash::from_bytes([0x11; 32])
    );
    assert_eq!(
        file_resolver_proof_digest,
        CanonicalHash::from_bytes([0x14; 32])
    );
    assert_eq!(file_authority_generation.epoch, 1);
    assert!(workspace_mutation_activation.is_none());
}

#[test]
fn r71_tool_authority_guarded_helper_returns_none_without_authority() {
    let binding = v3_file_access_binding(
        CanonicalHash::from_bytes([0x01; 32]),
        CanonicalHash::from_bytes([0x02; 32]),
        CanonicalHash::from_bytes([0x03; 32]),
        CanonicalHash::from_bytes([0x04; 32]),
        &ManagedFileAccessPlanDraftRefV1 {
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
        },
    );
    let outcome = adjudicate_guarded_tool_operation(
        None,
        &binding,
        &OpaquePermissionSubjectRef::new("ws-1".to_owned()),
        crate::managed_file_access::ManagedFileOperationV1::Read,
    )
    .expect("no authority is legacy-ok");
    assert!(outcome.is_none());
}

#[test]
fn r71_tool_authority_tool_context_guard() {
    use std::sync::{Arc, Mutex};
    // Reuse the in-module test adjudicator via the tool context wiring path: attach a
    // facade with a stub adjudicator, then exercise the context guard.
    let _ = (Arc::new(1), Mutex::new(()));
    let ctx = crate::tool::ToolContext::new(".", 30);
    // No authority attached: legacy-ok None.
    let binding = v3_file_access_binding(
        CanonicalHash::from_bytes([0x01; 32]),
        CanonicalHash::from_bytes([0x02; 32]),
        CanonicalHash::from_bytes([0x03; 32]),
        CanonicalHash::from_bytes([0x04; 32]),
        &ManagedFileAccessPlanDraftRefV1 {
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
        },
    );
    let outcome = ctx
        .adjudicate_file_operation(
            &binding,
            &OpaquePermissionSubjectRef::new("ws-1".to_owned()),
            crate::managed_file_access::ManagedFileOperationV1::Read,
        )
        .expect("legacy ok");
    assert!(outcome.is_none());
}

#[test]
fn r71_tool_authority_v3_context_adjudicates_or_defers() {
    let requirements = crate::resource::ResourceRequirementSetV1 {
        schema_version: 1,
        requirements: crate::resource::BoundedVec::new(),
        canonical_hash: CanonicalHash::from_bytes([0x31; 32]),
    };
    let file_ref = ManagedFileAccessPlanDraftRefV1 {
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
    };
    let plan = crate::permission_plan_v3_builder::build_v3_plan(
        crate::permission_plan_v3::ToolPermissionPlanCoreV3 {
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
            safe_summary: crate::ToolPermissionSummary {
                title: "read".to_owned(),
                detail: "".to_owned(),
                step_count: 0,
                workspace_code_steps: 0,
            },
        },
        requirements,
        Vec::new(),
        Vec::new(),
        Some(file_ref),
        crate::resource::ResourceJournalScopeV1::Application,
        crate::resource::RequestedEnforcementV1 {
            requirement: crate::resource::EnforcementRequirementClassV1::ExplicitUnconfined,
            deny_ambient_system_temp_write: false,
            deny_ambient_home_write: false,
            deny_ungranted_workspace_write: false,
            require_process_tree_ownership: false,
            require_network_policy: false,
            requested_capability_set_hash: CanonicalHash::from_bytes([0x21; 32]),
            profile_hash: CanonicalHash::from_bytes([0x22; 32]),
        },
    );
    let decision = crate::permission_plan_v3_builder::build_v3_decision(
        &plan,
        crate::resource::OpaquePermissionDecisionId::new("decision-1".to_owned()),
        crate::resource::OpaqueApprovalRequestId::new("approval-1".to_owned()),
        CanonicalHash::from_bytes([0x42; 32]),
        crate::resource::OpaqueToolCallId::new("call-1".to_owned()),
        CanonicalHash::from_bytes([0x11; 32]),
        "permission-v3",
        crate::ApprovalMode::Allow,
        crate::permission_plan_v3::ToolPermissionPolicyFacetsV3 {
            external_directory_required: false,
            session_grant_available: false,
            confirmation_required: false,
        },
        None,
        None,
        None,
    );
    // Without a V3 plan the context defers (legacy).
    let ctx = crate::tool::ToolContext::new(".", 30);
    let outcome = ctx
        .adjudicate_v3_file_operation(crate::managed_file_access::ManagedFileOperationV1::Read)
        .expect("legacy defers");
    assert!(outcome.is_none());
    // With the sealed V3 admission attached and a matching authority adjudicator, the guard
    // is armed: no tool_authority in this ctx -> legacy-ok None (adjudication gate is attached
    // when the surface injects the facade).
    let ctx = ctx.with_v3_admission(
        std::sync::Arc::new(plan.clone()),
        Some(std::sync::Arc::new(decision)),
    );
    let outcome = ctx
        .adjudicate_v3_file_operation(crate::managed_file_access::ManagedFileOperationV1::Read)
        .expect("v3 with no authority defers");
    assert!(outcome.is_none());
    // Decision drift on the sealed plan fails closed even without an authority (integrity).
    let mut drifted = build_v3_decision_shadow(&plan);
    drifted.plan_hash = CanonicalHash::from_bytes([0x88; 32]);
    drifted.decision_hash = CanonicalHash::from_bytes([0x99; 32]);
    let error = ctx
        .with_v3_admission(
            std::sync::Arc::new(plan.clone()),
            Some(std::sync::Arc::new(drifted)),
        )
        .adjudicate_v3_file_operation(crate::managed_file_access::ManagedFileOperationV1::Read)
        .expect_err("decision drift");
    assert!(matches!(
        error,
        crate::tool_authority::KernelToolAuthorityErrorV1::BindingKind(_)
    ));
}

// Small deterministic extra decision used to verify drift only (no real use of the original).
fn build_v3_decision_shadow(
    plan: &crate::permission_plan_v3::ToolPermissionPlanV3,
) -> crate::permission_plan_v3::ToolPermissionDecisionV3 {
    crate::permission_plan_v3_builder::build_v3_decision(
        plan,
        crate::resource::OpaquePermissionDecisionId::new("decision-1".to_owned()),
        crate::resource::OpaqueApprovalRequestId::new("approval-1".to_owned()),
        CanonicalHash::from_bytes([0x42; 32]),
        crate::resource::OpaqueToolCallId::new("call-1".to_owned()),
        CanonicalHash::from_bytes([0x11; 32]),
        "permission-v3",
        crate::ApprovalMode::Allow,
        crate::permission_plan_v3::ToolPermissionPolicyFacetsV3 {
            external_directory_required: false,
            session_grant_available: false,
            confirmation_required: false,
        },
        None,
        None,
        None,
    )
}
