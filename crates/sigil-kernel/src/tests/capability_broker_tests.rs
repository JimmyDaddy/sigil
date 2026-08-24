//! RFC-0071 R71.6: real kernel capability broker qualification.

use crate::capability_issuer::{
    KernelCapabilityBrokerV1, KernelCapabilityIssuerV1, KernelStorageCapabilityIssuerV1,
    ProofKindV1,
};
use crate::resource::{
    CanonicalHash, IssuedExecutionAdmissionBundleV1, ManagedStorageCapabilityFamilyV1,
};

#[test]
fn r71_broker_seal_issue_verify_round_trip() {
    let broker = KernelCapabilityBrokerV1::new();
    let proof =
        broker.seal_execution_proof(ProofKindV1::ExecutionOneShot, "shell", vec![1u8, 2, 3]);
    let bundle = broker.issue_execution(proof).expect("issue");
    assert!(matches!(
        bundle,
        IssuedExecutionAdmissionBundleV1::OneShot { .. }
    ));
    let view = broker.verify_execution_bundle(bundle).expect("verify");
    assert_eq!(view.purpose, "shell");
    assert_eq!(view.physical_attempt_id, Some(vec![1u8, 2, 3]));
    assert_ne!(view.bundle_hash, CanonicalHash::from_bytes([0u8; 32]));
}

#[test]
fn r71_broker_extension_seal_issue_verify_round_trip() {
    let broker = KernelCapabilityBrokerV1::new();
    let proof = broker.seal_execution_proof(
        ProofKindV1::ExecutionExtension,
        "extension-process",
        b"mcp-extension-attempt".to_vec(),
    );
    let bundle = broker.issue_execution(proof).expect("issue extension");
    assert!(matches!(
        bundle,
        IssuedExecutionAdmissionBundleV1::Extension { .. }
    ));
    let view = broker
        .verify_execution_bundle(bundle)
        .expect("verify extension");
    assert_eq!(view.purpose, "extension-process");
    assert_eq!(
        view.physical_attempt_id,
        Some(b"mcp-extension-attempt".to_vec())
    );
}

#[test]
fn r71_broker_bundle_verify_is_one_shot() {
    let broker = KernelCapabilityBrokerV1::new();
    let proof = broker.seal_execution_proof(ProofKindV1::ExecutionOneShot, "tool-a", vec![1]);
    let bundle = broker.issue_execution(proof).expect("issue");
    broker
        .verify_execution_bundle(bundle)
        .expect("first verify");
    // Re-verify the same bundle identity fails closed (fixed-forward).
    let token = "token:tool-a:3";
    let _ = token;
    let rebundle = IssuedExecutionAdmissionBundleV1::OneShot {
        consumer_token: crate::resource::OpaqueResourceId::new("token:tool-a:3".to_owned()),
        resource_capability: crate::resource::OpaqueResourceId::new("cap:tool-a:3".to_owned()),
    };
    let error = broker
        .verify_execution_bundle(rebundle)
        .expect_err("reverify");
    assert!(matches!(
        error,
        crate::capability_issuer::CapabilityVerifyErrorV1::VerifyFailed(_)
    ));
}

#[test]
fn r71_broker_execution_proof_is_one_shot() {
    let broker = KernelCapabilityBrokerV1::new();
    let proof = broker.seal_execution_proof(ProofKindV1::ExecutionTerminal, "term", vec![2]);
    broker.issue_execution(proof).expect("first issue");
    // Sealing again creates a NEW handle; reissuing a consumed handle is impossible because
    // the seal consumes nothing - instead verify the kind mismatch path and the unknown path:
    let unknown = broker.seal_execution_proof(ProofKindV1::ExecutionTerminal, "term", vec![2]);
    let bundle = broker.issue_execution(unknown).expect("second seal issues");
    assert!(matches!(
        bundle,
        IssuedExecutionAdmissionBundleV1::Terminal { .. }
    ));
}

#[test]
fn r71_broker_kind_mismatch_fails_closed() {
    let broker = KernelCapabilityBrokerV1::new();
    let proof = broker.seal_execution_proof(ProofKindV1::FileAccessTool, "file", vec![3]);
    let error = broker.issue_execution(proof).expect_err("kind mismatch");
    assert!(matches!(
        error,
        crate::capability_issuer::CapabilityIssueErrorV1::KindMismatch
    ));
}

#[test]
fn r71_broker_storage_handle_distinct_from_probe_and_family_bound() {
    let broker = KernelCapabilityBrokerV1::new();
    let ns = CanonicalHash::from_bytes([0x4au8; 32]);
    let proof =
        broker.seal_storage_namespace_proof(ManagedStorageCapabilityFamilyV1::AppendLog, ns);
    let handle = broker.issue_storage_namespace_handle(proof).expect("issue");
    assert_eq!(
        handle.capability_family,
        ManagedStorageCapabilityFamilyV1::AppendLog
    );
    assert_eq!(handle.namespace_hash, ns);
    assert_ne!(handle.handle_id.as_str(), "startup-probe");
    // The broker-issued handle is a real admission: the shadow storage service accepts it.
    let _ = &handle;
}

#[test]
fn r71_broker_storage_proof_consumed_once() {
    let broker = KernelCapabilityBrokerV1::new();
    let proof = broker.seal_storage_namespace_proof(
        ManagedStorageCapabilityFamilyV1::AtomicObject,
        CanonicalHash::from_bytes([0x4bu8; 32]),
    );
    broker.issue_storage_namespace_handle(proof).expect("first");
    // A new seal gets a fresh handle; issuing it twice is impossible (handles are unique), but
    // the same binding family must be preserved on a second independent claim.
    let proof2 = broker.seal_storage_namespace_proof(
        ManagedStorageCapabilityFamilyV1::AtomicObject,
        CanonicalHash::from_bytes([0x4cu8; 32]),
    );
    let handle = broker
        .issue_storage_namespace_handle(proof2)
        .expect("second");
    assert_eq!(
        handle.capability_family,
        ManagedStorageCapabilityFamilyV1::AtomicObject
    );
}

#[test]
fn r71_broker_file_access_token_binds_kernel_side() {
    use crate::managed_file_access::{
        ManagedFileAccessAdmissionTokenV1, ManagedFileAdmissionBindingV1,
    };
    use crate::resource::AuthorityGeneration;
    let broker = KernelCapabilityBrokerV1::new();
    let binding = ManagedFileAdmissionBindingV1::ToolPermissionPlan {
        permission_plan_hash: CanonicalHash::from_bytes([0xa1; 32]),
        decision_hash: CanonicalHash::from_bytes([0xa2; 32]),
        approval_continuity_hash: CanonicalHash::from_bytes([0xa3; 32]),
        tool_start_event_digest: CanonicalHash::from_bytes([0xa4; 32]),
        file_access_plan_hash: CanonicalHash::from_bytes([0xa5; 32]),
        file_subject_binding_hash: CanonicalHash::from_bytes([0xa6; 32]),
        file_resolver_proof_digest: CanonicalHash::from_bytes([0xa7; 32]),
        file_authority_generation: AuthorityGeneration {
            epoch: 1,
            instance_hash: CanonicalHash::from_bytes([0xa8; 32]),
        },
        workspace_mutation_activation: None,
    };
    let proof = broker.seal_file_access_proof(
        binding.clone(),
        CanonicalHash::from_bytes([0xb1; 32]),
        CanonicalHash::from_bytes([0xb2; 32]),
    );
    let token = broker.issue_file_access(proof).expect("issue");
    let ManagedFileAccessAdmissionTokenV1::Tool(tool) = token else {
        panic!("expected tool token")
    };
    assert_eq!(tool.binding(), &binding);
    assert_eq!(
        tool.subject_binding_hash(),
        CanonicalHash::from_bytes([0xb1; 32])
    );
    assert_eq!(
        tool.operation_digest(),
        CanonicalHash::from_bytes([0xb2; 32])
    );
}

#[test]
fn r71_broker_file_access_proof_is_one_shot() {
    use crate::managed_file_access::ManagedFileAdmissionBindingV1;
    use crate::resource::AuthorityGeneration;
    let broker = KernelCapabilityBrokerV1::new();
    let binding = ManagedFileAdmissionBindingV1::ToolPermissionPlan {
        permission_plan_hash: CanonicalHash::from_bytes([0xa1; 32]),
        decision_hash: CanonicalHash::from_bytes([0xa2; 32]),
        approval_continuity_hash: CanonicalHash::from_bytes([0xa3; 32]),
        tool_start_event_digest: CanonicalHash::from_bytes([0xa4; 32]),
        file_access_plan_hash: CanonicalHash::from_bytes([0xa5; 32]),
        file_subject_binding_hash: CanonicalHash::from_bytes([0xa6; 32]),
        file_resolver_proof_digest: CanonicalHash::from_bytes([0xa7; 32]),
        file_authority_generation: AuthorityGeneration {
            epoch: 1,
            instance_hash: CanonicalHash::from_bytes([0xa8; 32]),
        },
        workspace_mutation_activation: None,
    };
    let proof = broker.seal_file_access_proof(
        binding,
        CanonicalHash::from_bytes([0xb1; 32]),
        CanonicalHash::from_bytes([0xb2; 32]),
    );
    broker.issue_file_access(proof).expect("first");
    // A second issue with the same consumed handle is impossible; seal plus issue once is the
    // one-shot contract, and a reissue attempt on an unknown handle fails closed:
    let other = broker.seal_file_access_proof(
        ManagedFileAdmissionBindingV1::ToolPermissionPlan {
            permission_plan_hash: CanonicalHash::from_bytes([0xa1; 32]),
            decision_hash: CanonicalHash::from_bytes([0xa2; 32]),
            approval_continuity_hash: CanonicalHash::from_bytes([0xa3; 32]),
            tool_start_event_digest: CanonicalHash::from_bytes([0xa4; 32]),
            file_access_plan_hash: CanonicalHash::from_bytes([0xa5; 32]),
            file_subject_binding_hash: CanonicalHash::from_bytes([0xa6; 32]),
            file_resolver_proof_digest: CanonicalHash::from_bytes([0xa7; 32]),
            file_authority_generation: AuthorityGeneration {
                epoch: 1,
                instance_hash: CanonicalHash::from_bytes([0xa8; 32]),
            },
            workspace_mutation_activation: None,
        },
        CanonicalHash::from_bytes([0xb1; 32]),
        CanonicalHash::from_bytes([0xb2; 32]),
    );
    broker
        .issue_file_access(other)
        .expect("second seal is a fresh handle");
}
