use super::*;
use crate::resource::{CanonicalHash, OpaqueBlockerId, OpaqueResourceId, ResourceCleanupStatusV1};

fn sample_contract() -> ResourceRecoverySurfaceContractV1 {
    let blocker = PublicRecoveryBlockerV2 {
        blocker_id: OpaqueBlockerId::new("blocker-1".to_owned()),
        domain: ResourceRecoveryDomainV1::ManagedResource {
            resource_id: OpaqueResourceId::new("r1".to_owned()),
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
        schema_version: RESOURCE_RECOVERY_SURFACE_SCHEMA_VERSION,
        blocker: Some(blocker),
        resource_effects: vec![ResourceEffectReceiptViewV1 {
            resource_id: OpaqueResourceId::new("r1".to_owned()),
            cleanup_status: ResourceCleanupStatusV1::CleanupIncomplete {
                evidence_digest: CanonicalHash::from_bytes([1u8; 32]),
            },
            usage_bytes: 128,
            effect_settlement: crate::recovery::EffectSettlementV1::Applied,
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
fn r71_surface_lossless_round_trip() {
    let contract = sample_contract();
    contract.validate_schema().expect("valid");
    // JSON round-trip keeps every field exactly (no second state or hash).
    let encoded = serde_json::to_string(&contract).expect("encode");
    let decoded: ResourceRecoverySurfaceContractV1 =
        serde_json::from_str(&encoded).expect("decode");
    assert_eq!(decoded, contract);
}

#[test]
fn r71_surface_unknown_version_fails_closed() {
    let mut contract = sample_contract();
    contract.schema_version = 99;
    let error = contract.validate_schema().expect_err("unknown");
    assert!(matches!(
        error,
        SurfaceContractErrorV1::UnknownSchemaVersion { version: 99 }
    ));
}

#[test]
fn r71_surface_public_blocker_has_closed_domain() {
    let contract = sample_contract();
    let blocker = contract.blocker.expect("blocker");
    assert!(matches!(
        blocker.domain,
        ResourceRecoveryDomainV1::ManagedResource { .. }
    ));
}

#[test]
fn r71_surface_bootstrap_recovery_action_is_transport_neutral() {
    let envelope = ResourceRecoveryActionEnvelopeV1 {
        blocker_id: OpaqueBlockerId::new("bootstrap-corrupt".to_owned()),
        action: ResourceRecoveryActionV1::SelectFreshAuthorityEpoch,
        binding_hash: CanonicalHash::from_bytes([7u8; 32]),
    };
    let blocker = PublicRecoveryBlockerV2 {
        blocker_id: envelope.blocker_id.clone(),
        domain: ResourceRecoveryDomainV1::AuthorityBootstrap,
        reason_code: ResourceRecoveryReasonCodeV1::AuthorityBootstrapCorrupted,
        retry_disposition: ResourceRecoveryRetryDispositionV1::UserConfirmationRequired,
        action_envelope: Some(envelope.clone()),
        frontier_hash: CanonicalHash::from_bytes([8u8; 32]),
    };
    let contract = ResourceRecoverySurfaceContractV1 {
        schema_version: RESOURCE_RECOVERY_SURFACE_SCHEMA_VERSION,
        blocker: Some(blocker),
        resource_effects: Vec::new(),
        action_envelope: Some(envelope),
    };
    contract.validate_schema().expect("current schema");
    let round_trip: ResourceRecoverySurfaceContractV1 =
        serde_json::from_str(&serde_json::to_string(&contract).expect("encode")).expect("decode");
    assert_eq!(round_trip, contract);
}
