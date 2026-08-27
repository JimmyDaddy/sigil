//! RFC-0071 section 9.4 / R71.4: runtime transitional application-facing facade.
//!
//! Projects the kernel-owned ResourceRecoverySurfaceContractV1 losslessly to TUI/Desktop/CLI/HTTP.
//! This facade owns no second durable state, canonical hash or recovery policy; it is the
//! transitional edge that RFC-0070 R70.4/R70.6 replaces mechanically.

use sigil_kernel::resource::{CanonicalHash, OpaqueBlockerId};
use sigil_kernel::resource_recovery_surface::{
    ResourceRecoveryActionEnvelopeV1, ResourceRecoverySurfaceContractV1,
};

fn canonical_contract_hash(contract: &ResourceRecoverySurfaceContractV1) -> String {
    let encoded = serde_json::to_vec(contract).expect("recovery surface contract is serializable");
    format!("sha256:{}", sigil_kernel::sha256_hex(&encoded))
}

fn canonical_binding_hash(
    contract: &ResourceRecoverySurfaceContractV1,
    envelope: &ResourceRecoveryActionEnvelopeV1,
) -> String {
    let encoded = serde_json::to_vec(&(contract, envelope))
        .expect("recovery surface binding is serializable");
    format!("sha256:{}", sigil_kernel::sha256_hex(&encoded))
}

/// Facade query result: a lossless projection plus the exact action envelope round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRecoverySurfaceProjectionV1 {
    pub contract: ResourceRecoverySurfaceContractV1,
    pub projection_hash: String,
}

/// Facade dispatch result: the surface returns exactly the envelope it received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRecoveryDispatchV1 {
    pub accepted_envelope: ResourceRecoveryActionEnvelopeV1,
    pub binding_hash: String,
}

/// Transitional facade: kernel contract in/out, no transport or physical type.
#[derive(Debug, Default, Clone, Copy)]
pub struct RuntimeResourceRecoveryFacadeV1;

impl RuntimeResourceRecoveryFacadeV1 {
    pub const fn new() -> Self {
        Self
    }

    /// Creates the transport-neutral action emitted for a corrupt authority bootstrap. Product
    /// surfaces may render/return this envelope, but no surface receives a path or recovery
    /// credential; the independent doctor service validates the eventual operator confirmation.
    #[must_use]
    pub fn bootstrap_recovery_action(
        blocker_id: OpaqueBlockerId,
        binding_hash: CanonicalHash,
    ) -> ResourceRecoveryActionEnvelopeV1 {
        ResourceRecoveryActionEnvelopeV1 {
            blocker_id,
            action: sigil_kernel::resource_recovery_surface::ResourceRecoveryActionV1::SelectFreshAuthorityEpoch,
            binding_hash,
        }
    }

    /// Lossless projection: validates the kernel contract and binds the projection to its exact
    /// canonical bytes. The hash is correlation metadata only; authorization remains owned by
    /// the kernel action envelope.
    pub fn project(
        &self,
        contract: ResourceRecoverySurfaceContractV1,
    ) -> Result<ResourceRecoverySurfaceProjectionV1, facade_error::FacadeErrorV1> {
        contract
            .validate_schema()
            .map_err(|error| facade_error::FacadeErrorV1::UnknownSchema(error.to_string()))?;
        let projection_hash = canonical_contract_hash(&contract);
        Ok(ResourceRecoverySurfaceProjectionV1 {
            contract,
            projection_hash,
        })
    }

    /// Dispatch: a surface sends back the exact envelope; the facade verifies it is the one it
    /// received (binding equality), and never interprets or re-hashes it.
    pub fn dispatch(
        &self,
        projected: &ResourceRecoverySurfaceProjectionV1,
        returned: ResourceRecoveryActionEnvelopeV1,
    ) -> Result<ResourceRecoveryDispatchV1, facade_error::FacadeErrorV1> {
        let Some(expected) = projected.contract.action_envelope.as_ref() else {
            return Err(facade_error::FacadeErrorV1::NoActionEnvelope);
        };
        if *expected != returned {
            return Err(facade_error::FacadeErrorV1::EnvelopeMismatch);
        }
        let binding_hash = canonical_binding_hash(&projected.contract, &returned);
        Ok(ResourceRecoveryDispatchV1 {
            accepted_envelope: returned,
            binding_hash,
        })
    }
}

/// Facade error classification (closed).
pub mod facade_error {
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum FacadeErrorV1 {
        #[error("surface contract schema rejected: {0}")]
        UnknownSchema(String),
        #[error("surface returned an envelope that was not the projected one")]
        EnvelopeMismatch,
        #[error("projected contract carries no action envelope")]
        NoActionEnvelope,
    }
}

#[cfg(test)]
mod tests {
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
    fn r71_facade_projects_and_dispatches_losslessly() {
        let facade = RuntimeResourceRecoveryFacadeV1::new();
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
    fn r71_facade_rejects_alien_envelope() {
        let facade = RuntimeResourceRecoveryFacadeV1::new();
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
    fn r71_facade_hashes_contract_and_binding_content_not_schema_only() {
        let facade = RuntimeResourceRecoveryFacadeV1::new();
        let first = facade.project(sample_contract()).expect("first project");
        let mut changed = sample_contract();
        changed.blocker.as_mut().expect("blocker").frontier_hash =
            CanonicalHash::from_bytes([8u8; 32]);
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
}
