//! RFC-0071: sandbox launch plan contract (R71.1 shape only).

use sigil_kernel::resource::{CanonicalHash, RequestedEnforcementV1};

/// Provider-neutral sealed launch plan: requested enforcement plus the manifest/profile hashes
/// that must match the approved permission plan. Effective enforcement is observed by the
/// backend receipt, never copied from the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedSandboxLaunchPlanV1 {
    pub launch_plan_hash: CanonicalHash,
    pub request_id: String,
    pub requested_enforcement: RequestedEnforcementV1,
    pub manifest_hash: CanonicalHash,
    pub profile_hash: CanonicalHash,
}
