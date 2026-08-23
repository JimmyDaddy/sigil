//! RFC-0071 section 9.4 / R71.4: runtime transitional application-facing facade.
//!
//! Projects the kernel-owned ResourceRecoverySurfaceContractV1 losslessly to TUI/Desktop/CLI/HTTP.
//! This facade owns no second durable state, canonical hash or recovery policy; it is the
//! transitional edge that RFC-0070 R70.4/R70.6 replaces mechanically.

use sigil_kernel::resource_recovery_surface::{
    ResourceEffectReceiptViewV1, ResourceRecoveryActionEnvelopeV1,
    ResourceRecoverySurfaceContractV1,
};

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

    /// Lossless projection: recomputes schema validation and returns the exact contract bytes
    /// definition; surfaces never re-canonicalize.
    pub fn project(
        &self,
        contract: ResourceRecoverySurfaceContractV1,
    ) -> Result<ResourceRecoverySurfaceProjectionV1, facade_error::FacadeErrorV1> {
        contract
            .validate_schema()
            .map_err(|error| facade_error::FacadeErrorV1::UnknownSchema(error.to_string()))?;
        let projection_hash = format!("facade-v1:{:x}", contract.schema_version);
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
        Ok(ResourceRecoveryDispatchV1 {
            accepted_envelope: returned,
            binding_hash: format!("binding:{:x}", projected.contract.schema_version),
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
}
