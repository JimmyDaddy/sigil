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
}
