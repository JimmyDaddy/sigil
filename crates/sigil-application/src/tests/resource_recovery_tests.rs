use super::*;
use sigil_kernel::resource::{CanonicalHash, OpaqueBlockerId, ResourceCleanupStatusV1};
use sigil_kernel::resource_recovery_surface::ResourceEffectReceiptViewV1;
use sigil_kernel::resource_recovery_surface::{
    PublicRecoveryBlockerV2, ResourceRecoveryActionV1, ResourceRecoveryDomainV1,
    ResourceRecoveryReasonCodeV1, ResourceRecoveryRetryDispositionV1,
};

fn sample_contract() -> ResourceRecoverySurfaceContractV1 {
    let blocker = PublicRecoveryBlockerV2 {
        blocker_id: OpaqueBlockerId::new("blocker-1".to_owned()),
        domain: ResourceRecoveryDomainV1::ManagedResource {
            resource_id: sigil_kernel::resource::OpaqueResourceId::new("r1".to_owned()),
            cleanup_status: ResourceCleanupStatusV1::CleanupIncomplete {
                evidence_digest: CanonicalHash::from_bytes([1u8; 32]),
            },
        },
        reason_code: ResourceRecoveryReasonCodeV1::CleanupIncomplete,
        retry_disposition: ResourceRecoveryRetryDispositionV1::BlockedUntilResolved,
        action_envelope: Some(ResourceRecoveryActionEnvelopeV1 {
            blocker_id: OpaqueBlockerId::new("blocker-1".to_owned()),
            action: ResourceRecoveryActionV1::ReconcileCleanupIncomplete,
            binding_hash: CanonicalHash::from_bytes([2u8; 32]),
        }),
        frontier_hash: CanonicalHash::from_bytes([3u8; 32]),
    };
    ResourceRecoverySurfaceContractV1 {
        schema_version: 1,
        blocker: Some(blocker),
        resource_effects: vec![ResourceEffectReceiptViewV1 {
            resource_id: sigil_kernel::resource::OpaqueResourceId::new("r1".to_owned()),
            cleanup_status: ResourceCleanupStatusV1::CleanupIncomplete {
                evidence_digest: CanonicalHash::from_bytes([1u8; 32]),
            },
            usage_bytes: 128,
            effect_settlement: sigil_kernel::recovery::EffectSettlementV1::Applied,
            receipt_hash: CanonicalHash::from_bytes([4u8; 32]),
        }],
        action_envelope: Some(ResourceRecoveryActionEnvelopeV1 {
            blocker_id: OpaqueBlockerId::new("blocker-1".to_owned()),
            action: ResourceRecoveryActionV1::ReconcileCleanupIncomplete,
            binding_hash: CanonicalHash::from_bytes([2u8; 32]),
        }),
    }
}

#[test]
fn application_facade_projects_and_dispatches_losslessly() {
    let facade = ApplicationResourceRecoveryFacadeV1::new();
    let projected = facade.project(sample_contract()).expect("project");
    let returned = projected
        .contract
        .action_envelope
        .clone()
        .expect("envelope");
    let dispatched = facade.dispatch(&projected, returned).expect("dispatch");
    assert_eq!(
        dispatched.accepted_envelope.action,
        ResourceRecoveryActionV1::ReconcileCleanupIncomplete
    );
}

#[test]
fn application_facade_rejects_alien_envelope() {
    let facade = ApplicationResourceRecoveryFacadeV1::new();
    let projected = facade.project(sample_contract()).expect("project");
    let alien = ResourceRecoveryActionEnvelopeV1 {
        blocker_id: OpaqueBlockerId::new("other".to_owned()),
        action: ResourceRecoveryActionV1::ResetQuarantinedGeneration,
        binding_hash: CanonicalHash::from_bytes([9u8; 32]),
    };
    let error = facade.dispatch(&projected, alien).expect_err("must fail");
    assert!(matches!(
        error,
        facade_error::FacadeErrorV1::EnvelopeMismatch
    ));
}

#[test]
fn application_facade_hashes_contract_and_binding_content_not_schema_only() {
    let facade = ApplicationResourceRecoveryFacadeV1::new();
    let first = facade.project(sample_contract()).expect("first project");
    let mut changed = sample_contract();
    changed.blocker.as_mut().expect("blocker").frontier_hash = CanonicalHash::from_bytes([8u8; 32]);
    let second = facade.project(changed).expect("second project");

    assert_ne!(first.projection_hash, second.projection_hash);
    let first_envelope = first.contract.action_envelope.clone().expect("envelope");
    let second_envelope = second.contract.action_envelope.clone().expect("envelope");
    let first_dispatch = facade
        .dispatch(&first, first_envelope)
        .expect("first dispatch");
    let second_dispatch = facade
        .dispatch(&second, second_envelope)
        .expect("second dispatch");
    assert_ne!(first_dispatch.binding_hash, second_dispatch.binding_hash);
}
