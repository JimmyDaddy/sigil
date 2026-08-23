//! RFC-0071 section I71.15: renderer-neutral resource recovery surface contract.
//!
//! This is the versioned, transport-neutral contract every product surface (TUI / Desktop / CLI /
//! HTTP) projects from. It contains no renderer/transport type, no PathBuf, no authority/sandbox
//! concrete type, and no runtime-private type. Unknown schema versions fail closed.

use serde::{Deserialize, Serialize};

use crate::recovery::EffectSettlementV1;
use crate::resource::{CanonicalHash, OpaqueBlockerId, OpaqueResourceId, ResourceCleanupStatusV1};

pub const RESOURCE_RECOVERY_SURFACE_SCHEMA_VERSION: u32 = 1;

/// Closed reason-code classification for resource recovery projections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceRecoveryReasonCodeV1 {
    ProvisioningFailed,
    MeasurementLimitExceeded,
    SymlinkDetected,
    UnsupportedEntry,
    QuotaExceeded,
    CleanupIncomplete,
    Quarantined,
    OutcomeUncertain,
}

/// Closed retry disposition (never inferred from an error string).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceRecoveryRetryDispositionV1 {
    RetryExact,
    ReplanRequired,
    UserConfirmationRequired,
    BlockedUntilResolved,
    NotRetryable,
}

/// Closed recovery action envelope: the only language a surface sends back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRecoveryActionEnvelopeV1 {
    pub blocker_id: OpaqueBlockerId,
    pub action: ResourceRecoveryActionV1,
    pub binding_hash: CanonicalHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceRecoveryActionV1 {
    ResetQuarantinedGeneration,
    ReconcileCleanupIncomplete,
    RecreateExecutionTemp,
    UserReselectDestination,
}

/// Public blocker projection shared by all surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicRecoveryBlockerV2 {
    pub blocker_id: OpaqueBlockerId,
    pub domain: ResourceRecoveryDomainV1,
    pub reason_code: ResourceRecoveryReasonCodeV1,
    pub retry_disposition: ResourceRecoveryRetryDispositionV1,
    pub action_envelope: Option<ResourceRecoveryActionEnvelopeV1>,
    pub frontier_hash: CanonicalHash,
}

/// Closed blocker domain classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceRecoveryDomainV1 {
    ManagedResource {
        resource_id: OpaqueResourceId,
        cleanup_status: ResourceCleanupStatusV1,
    },
    Requirement,
    RealizedGeneration,
    Storage,
    Maintenance,
}

/// Resource / effect receipt projection (lossless view, never a second state).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceEffectReceiptViewV1 {
    pub resource_id: OpaqueResourceId,
    pub cleanup_status: ResourceCleanupStatusV1,
    pub usage_bytes: u64,
    pub effect_settlement: EffectSettlementV1,
    pub receipt_hash: CanonicalHash,
}

/// The complete surface contract document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRecoverySurfaceContractV1 {
    pub schema_version: u32,
    pub blocker: Option<PublicRecoveryBlockerV2>,
    pub resource_effects: Vec<ResourceEffectReceiptViewV1>,
    pub action_envelope: Option<ResourceRecoveryActionEnvelopeV1>,
}

impl ResourceRecoverySurfaceContractV1 {
    /// Unknown versions fail closed before any projection or dispatch.
    pub fn validate_schema(&self) -> Result<(), SurfaceContractErrorV1> {
        if self.schema_version != RESOURCE_RECOVERY_SURFACE_SCHEMA_VERSION {
            return Err(SurfaceContractErrorV1::UnknownSchemaVersion {
                version: self.schema_version,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SurfaceContractErrorV1 {
    #[error("unknown resource recovery surface schema version: {version}")]
    UnknownSchemaVersion { version: u32 },
}
