//! RFC-0071 R71.6: kernel tool authority facade qualification.

use std::sync::{Arc, Mutex};

use crate::capability_issuer::KernelCapabilityBrokerV1;
use crate::managed_file_access::ManagedFileAccessServiceV1;

/// Minimal in-test adjudicator: counts calls and rejects unregistered subjects (the real
/// adjudicator lives in sigil-resource-authority; this kernel-side fixture proves the
/// facade's seal -> issue -> access chain).
struct TestAdjudicatorV1 {
    observed: Mutex<std::collections::BTreeSet<String>>,
    calls: Mutex<u64>,
}

#[allow(dead_code)]
impl TestAdjudicatorV1 {
    fn new(observed: &[&str]) -> Self {
        Self {
            observed: Mutex::new(observed.iter().map(|entry| (*entry).to_owned()).collect()),
            calls: Mutex::new(0),
        }
    }
}

impl ManagedFileAccessServiceV1 for TestAdjudicatorV1 {
    fn access(
        &self,
        request: crate::managed_file_access::ManagedFileAccessRequestV1,
        token: crate::managed_file_access::ManagedFileAccessAdmissionTokenV1,
    ) -> Result<
        crate::managed_file_access::ManagedFileAccessResultV1,
        crate::managed_file_access::ManagedFileAccessErrorV1,
    > {
        *self.calls.lock().expect("calls lock") += 1;
        let observed = self.observed.lock().expect("observed lock");
        if !observed.contains(request.subject_ref.as_str()) {
            return Err(
                crate::managed_file_access::ManagedFileAccessErrorV1::OperationNotPermitted,
            );
        }
        let _ = token;
        let digest = crate::resource::CanonicalHash::from_bytes([0x7du8; 32]);
        Ok(crate::managed_file_access::ManagedFileAccessResultV1 {
            access_receipt: crate::managed_execution::BorrowedResourceAccessReceiptV1 {
                subject_ref: request.subject_ref.clone(),
                subject_binding_hash: crate::resource::CanonicalHash::from_bytes([0x7eu8; 32]),
                operation_digest: request.operation_digest,
                granted_access_hash: crate::resource::CanonicalHash::from_bytes([0x7fu8; 32]),
                identity_before: None,
                identity_after: None,
                borrowed_effect_frontier_hash: digest,
                effect_settlement: crate::recovery::EffectSettlementV1::Applied,
                receipt_hash: digest,
            },
            effect_settlement: crate::recovery::EffectSettlementV1::Applied,
            result_digest: digest,
        })
    }
}

fn plan_binding() -> crate::managed_file_access::ManagedFileAdmissionBindingV1 {
    crate::managed_file_access::ManagedFileAdmissionBindingV1::ToolPermissionPlan {
        permission_plan_hash: crate::resource::CanonicalHash::from_bytes([0xa1; 32]),
        decision_hash: crate::resource::CanonicalHash::from_bytes([0xa2; 32]),
        approval_continuity_hash: crate::resource::CanonicalHash::from_bytes([0xa3; 32]),
        tool_start_event_digest: crate::resource::CanonicalHash::from_bytes([0xa4; 32]),
        file_access_plan_hash: crate::resource::CanonicalHash::from_bytes([0xa5; 32]),
        file_subject_binding_hash: crate::resource::CanonicalHash::from_bytes([0xa6; 32]),
        file_resolver_proof_digest: crate::resource::CanonicalHash::from_bytes([0xa7; 32]),
        file_authority_generation: crate::resource::AuthorityGeneration {
            epoch: 1,
            instance_hash: crate::resource::CanonicalHash::from_bytes([0xa8; 32]),
        },
        workspace_mutation_activation: None,
    }
}

#[test]
fn r71_tool_authority_adjudicates_with_one_shot_token() {
    let broker = Arc::new(KernelCapabilityBrokerV1::new());
    let adjudicator = Arc::new(TestAdjudicatorV1::new(&["ws-1"]));
    let authority = crate::tool_authority::KernelToolAuthorityV1::new(adjudicator.clone(), broker);
    let subject = crate::resource::OpaquePermissionSubjectRef::new("ws-1".to_owned());
    let result = authority
        .adjudicate_tool_file_access(
            plan_binding(),
            &subject,
            crate::managed_file_access::ManagedFileOperationV1::Read,
        )
        .expect("adjudicate");
    assert_eq!(
        result.effect_settlement,
        crate::recovery::EffectSettlementV1::Applied
    );
}

#[test]
fn r71_tool_authority_unregistered_subject_fails_closed() {
    let broker = Arc::new(KernelCapabilityBrokerV1::new());
    let adjudicator = Arc::new(TestAdjudicatorV1::new(&["ws-1"]));
    let authority = crate::tool_authority::KernelToolAuthorityV1::new(adjudicator, broker);
    let subject = crate::resource::OpaquePermissionSubjectRef::new("unknown".to_owned());
    let error = authority
        .adjudicate_tool_file_access(
            plan_binding(),
            &subject,
            crate::managed_file_access::ManagedFileOperationV1::Write,
        )
        .expect_err("unregistered");
    assert!(matches!(
        error,
        crate::tool_authority::KernelToolAuthorityErrorV1::Access(
            crate::managed_file_access::ManagedFileAccessErrorV1::OperationNotPermitted
        )
    ));
}
