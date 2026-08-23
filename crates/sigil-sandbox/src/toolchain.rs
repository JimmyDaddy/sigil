//! RFC-0071 section 11.2: toolchain binding plan and sanitized config view.
//!
//! Fresh HOME must not break the base toolchain. The Environment Profile Resolver performs only
//! side-effect-free observation and logical planning before permission; it never fabricates a
//! ResourceRefV1 for a ToolCache or sanitized config that has not been allowed/acquired.

use sigil_kernel::resource::{CanonicalHash, EnvironmentProfileClassV1, ResourceRefV1};
#[cfg(test)]
use sigil_kernel::resource::{OpaqueResourceId, ResourceKindV1};

/// Closed toolchain family classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolchainFamilyV1 {
    Rust,
    Node,
    Git,
    GenericExecutable,
}

/// Borrowed observation: opaque subject / safe label / identity digest only, no access grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorrowedResourceObservationV1 {
    pub subject_ref: String,
    pub safe_label: String,
    pub identity_digest: CanonicalHash,
}

/// Side-effect-free toolchain binding plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainBindingPlanV1 {
    pub family: ToolchainFamilyV1,
    pub executable_observation: BorrowedResourceObservationV1,
    pub readonly_store_observations: Vec<BorrowedResourceObservationV1>,
    pub managed_requirement_keys: Vec<String>,
    pub user_config_source_observations: Vec<BorrowedResourceObservationV1>,
    pub environment_plan: Vec<PlannedEnvironmentVariableV1>,
    pub plan_hash: CanonicalHash,
}

/// One planned environment variable (resolver never leaks secret config).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedEnvironmentVariableV1 {
    pub name: String,
    pub source: PlannedEnvSourceV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannedEnvSourceV1 {
    ExecutionTempStandard,
    ToolchainStore,
    ToolCache,
    SanitizedConfigView,
}

/// Sanitized config view: exact ExecutionTemp config/ subresource, never a new ResourceKindV1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedConfigViewRefV1 {
    pub view_id: String,
    pub parent_execution_temp: ResourceRefV1,
    pub source_subject_binding_hash: CanonicalHash,
    pub projection_policy_hash: CanonicalHash,
    pub view_binding_hash: CanonicalHash,
}

/// Realized toolchain binding (only after allow/acquire; CAS-matched to the plan).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealizedToolchainBindingV1 {
    pub family: ToolchainFamilyV1,
    pub executable_ref: ResourceRefV1,
    pub readonly_stores: Vec<ResourceRefV1>,
    pub managed_cache_refs: Vec<ResourceRefV1>,
    pub safe_user_config_views: Vec<SanitizedConfigViewRefV1>,
    pub environment: Vec<PlannedEnvironmentVariableV1>,
    pub source_plan_hash: CanonicalHash,
    pub binding_hash: CanonicalHash,
}

/// Closed toolchain error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolchainErrorV1 {
    #[error("toolchain executable observation failed: {0}")]
    ObservationFailed(String),
    #[error("realized binding does not match the approved plan (CAS)")]
    BindingDrift,
    #[error("toolchain cache materialization unavailable; user approval required")]
    CacheMaterializationUnavailable,
}

/// Validates a realized binding against its approved plan (exact CAS).
pub fn validate_realized_binding(
    plan: &ToolchainBindingPlanV1,
    realized: &RealizedToolchainBindingV1,
) -> Result<(), ToolchainErrorV1> {
    if plan.family != realized.family {
        return Err(ToolchainErrorV1::BindingDrift);
    }
    if plan.plan_hash != realized.source_plan_hash {
        return Err(ToolchainErrorV1::BindingDrift);
    }
    Ok(())
}

/// Fresh-isolated-home profile classifier (trivial closed check).
pub fn fresh_isolated_home_profile() -> EnvironmentProfileClassV1 {
    EnvironmentProfileClassV1::FreshIsolatedHomeWithToolchain
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> ToolchainBindingPlanV1 {
        ToolchainBindingPlanV1 {
            family: ToolchainFamilyV1::Rust,
            executable_observation: BorrowedResourceObservationV1 {
                subject_ref: "toolchain:cargo".to_owned(),
                safe_label: "cargo".to_owned(),
                identity_digest: CanonicalHash::from_bytes([1u8; 32]),
            },
            readonly_store_observations: Vec::new(),
            managed_requirement_keys: vec!["tool-cache-rust".to_owned()],
            user_config_source_observations: Vec::new(),
            environment_plan: vec![PlannedEnvironmentVariableV1 {
                name: "CARGO_HOME".to_owned(),
                source: PlannedEnvSourceV1::ToolCache,
            }],
            plan_hash: CanonicalHash::from_bytes([2u8; 32]),
        }
    }

    #[test]
    fn r71_toolchain_realized_binding_must_match_plan_cas() {
        let plan = plan();
        let mut realized = RealizedToolchainBindingV1 {
            family: ToolchainFamilyV1::Rust,
            executable_ref: ResourceRefV1 {
                resource_id: OpaqueResourceId::new("exec-1".to_owned()),
                kind: ResourceKindV1::ToolchainStore,
                owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
                journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
                generation: 1,
            },
            readonly_stores: Vec::new(),
            managed_cache_refs: Vec::new(),
            safe_user_config_views: Vec::new(),
            environment: plan.environment_plan.clone(),
            source_plan_hash: plan.plan_hash,
            binding_hash: CanonicalHash::from_bytes([3u8; 32]),
        };
        validate_realized_binding(&plan, &realized).expect("match");
        realized.source_plan_hash = CanonicalHash::from_bytes([9u8; 32]);
        let error = validate_realized_binding(&plan, &realized).expect_err("drift");
        assert!(matches!(error, ToolchainErrorV1::BindingDrift));
    }
}
