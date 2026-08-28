//! Application-facing projection of the kernel-owned resource recovery surface.
//!
//! This module is deliberately stateless. It does not own a recovery policy, durable state,
//! canonical schema, physical resource, or transport. It validates and round-trips the exact
//! kernel contract so product surfaces can use the application boundary without a runtime
//! transitional facade.

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

/// Lossless application projection plus the exact action envelope round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRecoverySurfaceProjectionV1 {
    pub contract: ResourceRecoverySurfaceContractV1,
    pub projection_hash: String,
}

/// Application dispatch result for an envelope accepted by the projection binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRecoveryDispatchV1 {
    pub accepted_envelope: ResourceRecoveryActionEnvelopeV1,
    pub binding_hash: String,
}

/// Stateless application boundary for the kernel recovery surface.
#[derive(Debug, Default, Clone, Copy)]
pub struct ApplicationResourceRecoveryFacadeV1;

impl ApplicationResourceRecoveryFacadeV1 {
    pub const fn new() -> Self {
        Self
    }

    /// Creates the transport-neutral action emitted for a corrupt authority bootstrap. Product
    /// surfaces may render/return this envelope, but no surface receives a path or credential.
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

    /// Validates the kernel contract and binds the projection to its exact canonical bytes.
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

    /// Accepts only the exact envelope attached to the projected contract.
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

/// Closed error classification for the application recovery boundary.
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
#[path = "tests/resource_recovery_tests.rs"]
mod tests;
