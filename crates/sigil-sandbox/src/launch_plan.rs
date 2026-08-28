//! RFC-0071 section 8.4/11: sealed sandbox launch plan.
//!
//! The plan binds requested enforcement, manifest and profile hashes to the approved permission
//! plan. Effective enforcement is always observed by the backend receipt, never copied from the
//! request; any drift in the sealed binding fails before a platform call.

use sigil_kernel::resource::{CanonicalHash, RequestedEnforcementV1};

/// Provider-neutral sealed launch plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedSandboxLaunchPlanV1 {
    pub launch_plan_hash: CanonicalHash,
    pub request_id: String,
    pub requested_enforcement: RequestedEnforcementV1,
    pub manifest_hash: CanonicalHash,
    pub profile_hash: CanonicalHash,
}

/// Closed launch-plan drift error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaunchPlanErrorV1 {
    #[error("launch plan hash does not match its sealed fields")]
    PlanHashMismatch,
    #[error("launch plan references a stale request digest")]
    StaleRequest,
    #[error("launch plan is not bound to a registered sandbox provider")]
    UnregisteredProvider,
}

impl SealedSandboxLaunchPlanV1 {
    /// Builds the sealed plan with an exact hash (canonical field order).
    pub fn build(
        request_id: String,
        requested_enforcement: RequestedEnforcementV1,
        manifest_hash: CanonicalHash,
        profile_hash: CanonicalHash,
    ) -> Self {
        let digest = canonical_hash(&[
            b"launch-plan-v1".as_slice(),
            request_id.as_bytes(),
            &manifest_hash.as_bytes()[..],
            &profile_hash.as_bytes()[..],
        ]);
        Self {
            launch_plan_hash: digest,
            request_id,
            requested_enforcement,
            manifest_hash,
            profile_hash,
        }
    }

    /// Validates that the sealed hash equals the recomputed canonical hash.
    pub fn validate(&self) -> Result<(), LaunchPlanErrorV1> {
        let recomputed = canonical_hash(&[
            b"launch-plan-v1".as_slice(),
            self.request_id.as_bytes(),
            &self.manifest_hash.as_bytes()[..],
            &self.profile_hash.as_bytes()[..],
        ]);
        if recomputed != self.launch_plan_hash {
            return Err(LaunchPlanErrorV1::PlanHashMismatch);
        }
        Ok(())
    }
}

/// Canonical sha256 digest.
pub fn canonical_hash(parts: &[&[u8]]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    CanonicalHash::from_bytes(hasher.finalize().into())
}

#[cfg(test)]
#[path = "tests/launch_plan_tests.rs"]
mod tests;
