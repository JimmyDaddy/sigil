//! RFC-0071 section 8.4 / 11.3: requested-vs-effective enforcement receipts.
//!
//! A provider never clones the requested access set into the receipt: the effective set comes
//! from backend observation/probe. Required-exact with overgrant fails closed; Local reports
//! none, never a fabricated subset.

use std::collections::BTreeSet;

use sigil_kernel::managed_execution::{
    AccessPolicySatisfactionV1, AccessWideningPolicyV1, AccessWideningReceiptV1,
    ResourceEnforcementReceiptV1,
};
use sigil_kernel::resource::{
    CanonicalHash, EnforcementCompletenessV1, ResourceAccessV1, ResourceRefV1,
    SandboxBackendClassV1,
};

/// Closed read isolation requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadIsolationRequirementV1 {
    AmbientReadAllowed,
    DenyUngrantReadRequired,
}

/// Closed read isolation completeness (truthful backend observation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadIsolationCompletenessV1 {
    Full,
    Partial { ambient_classes: Vec<String> },
    None,
}

/// Closed platform support state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxPlatformSupportV1 {
    Supported,
    Unsupported,
    DiagnosticOnly,
}

/// Closed enforcement verification error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnforcementVerificationErrorV1 {
    #[error("required-exact policy met with an overgrant; refusing spawn")]
    ExceededByOvergrant,
    #[error("provider observation is missing or ambiguous; refusing to fabricate")]
    ObservationMissing,
    #[error("required read isolation cannot be proven; refusing to claim full confinement")]
    ReadIsolationUnproven,
    #[error("backend is unavailable or incapable; no implicit Local fallback")]
    BackendUnavailable,
    #[error("Local may only run under ExplicitUnconfined policy")]
    LocalRequiresUnconfined,
}

/// Verifies a provider receipt against the requested access set (exact or declared superset).
pub fn verify_enforcement(
    resource: &ResourceRefV1,
    requested: &BTreeSet<ResourceAccessV1>,
    requested_policy: &AccessWideningPolicyV1,
    observed_effective: &BTreeSet<ResourceAccessV1>,
    backend: SandboxBackendClassV1,
    completeness: EnforcementCompletenessV1,
) -> Result<ResourceEnforcementReceiptV1, EnforcementVerificationErrorV1> {
    if backend == SandboxBackendClassV1::LocalUnconfined {
        if !matches!(requested_policy, AccessWideningPolicyV1::ExplicitUnconfined) {
            return Err(EnforcementVerificationErrorV1::LocalRequiresUnconfined);
        }
        // Local reports none, never a fabricated subset.
        return Ok(ResourceEnforcementReceiptV1 {
            resource_ref: resource.clone(),
            access: AccessWideningReceiptV1 {
                requested: requested.clone(),
                effective: BTreeSet::new(),
                unavoidable_widening: BTreeSet::new(),
                proof_digest: CanonicalHash::from_bytes([1u8; 32]),
            },
            requested_policy: AccessWideningPolicyV1::ExplicitUnconfined,
            policy_satisfaction: AccessPolicySatisfactionV1::ExplicitUnconfined,
            enforcement: EnforcementCompletenessV1::None,
            proof_digest: CanonicalHash::from_bytes([2u8; 32]),
        });
    }
    if completeness == EnforcementCompletenessV1::None {
        return Err(EnforcementVerificationErrorV1::ObservationMissing);
    }
    let widening: Vec<_> = observed_effective.difference(requested).cloned().collect();
    let widening_set: BTreeSet<_> = widening.iter().cloned().collect();
    let satisfaction = match requested_policy {
        AccessWideningPolicyV1::Exact
            if !widening_set.is_empty() || !observed_effective.is_superset(requested) =>
        {
            return Err(EnforcementVerificationErrorV1::ExceededByOvergrant);
        }
        AccessWideningPolicyV1::Exact => AccessPolicySatisfactionV1::Exact,
        AccessWideningPolicyV1::AllowDeclaredSuperset { .. }
            if observed_effective.is_superset(requested) =>
        {
            AccessPolicySatisfactionV1::DeclaredSuperset {
                declaration_hash: match requested_policy {
                    AccessWideningPolicyV1::AllowDeclaredSuperset { declaration_hash } => {
                        *declaration_hash
                    }
                    _ => CanonicalHash::from_bytes([0u8; 32]),
                },
            }
        }
        _ => return Err(EnforcementVerificationErrorV1::ExceededByOvergrant),
    };
    Ok(ResourceEnforcementReceiptV1 {
        resource_ref: resource.clone(),
        access: AccessWideningReceiptV1 {
            requested: requested.clone(),
            effective: observed_effective.clone(),
            unavoidable_widening: widening_set.clone(),
            proof_digest: CanonicalHash::from_bytes([3u8; 32]),
        },
        requested_policy: requested_policy.clone(),
        policy_satisfaction: satisfaction,
        enforcement: completeness,
        proof_digest: CanonicalHash::from_bytes([4u8; 32]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigil_kernel::resource::{
        OpaqueResourceId, ResourceJournalScopeV1, ResourceKindV1, ResourceOwnerScopeV1,
    };

    fn resource() -> ResourceRefV1 {
        ResourceRefV1 {
            resource_id: OpaqueResourceId::new("r1".to_owned()),
            kind: ResourceKindV1::ExecutionTemp,
            owner_scope: ResourceOwnerScopeV1::Application,
            journal_scope: ResourceJournalScopeV1::Application,
            generation: 1,
        }
    }

    #[test]
    fn r71_exact_policy_rejects_overgrant() {
        let requested = BTreeSet::from([ResourceAccessV1::Read]);
        let observed = BTreeSet::from([ResourceAccessV1::Read, ResourceAccessV1::Write]);
        let error = verify_enforcement(
            &resource(),
            &requested,
            &AccessWideningPolicyV1::Exact,
            &observed,
            SandboxBackendClassV1::LinuxBubblewrap,
            EnforcementCompletenessV1::Exact,
        )
        .expect_err("overgrant must fail");
        assert!(matches!(
            error,
            EnforcementVerificationErrorV1::ExceededByOvergrant
        ));
    }

    #[test]
    fn r71_declared_superset_is_recorded_not_fabricated() {
        let requested = BTreeSet::from([ResourceAccessV1::Read]);
        let observed = BTreeSet::from([ResourceAccessV1::Read, ResourceAccessV1::Execute]);
        let declaration = CanonicalHash::from_bytes([7u8; 32]);
        let receipt = verify_enforcement(
            &resource(),
            &requested,
            &AccessWideningPolicyV1::AllowDeclaredSuperset {
                declaration_hash: declaration,
            },
            &observed,
            SandboxBackendClassV1::MacOsSeatbelt,
            EnforcementCompletenessV1::Exact,
        )
        .expect("declared superset ok");
        assert_eq!(
            receipt.policy_satisfaction,
            AccessPolicySatisfactionV1::DeclaredSuperset {
                declaration_hash: declaration
            }
        );
        assert_eq!(receipt.access.unavoidable_widening.len(), 1);
    }

    #[test]
    fn r71_local_reports_none_never_a_subset() {
        let requested = BTreeSet::from([ResourceAccessV1::Read]);
        let receipt = verify_enforcement(
            &resource(),
            &requested,
            &AccessWideningPolicyV1::ExplicitUnconfined,
            &requested,
            SandboxBackendClassV1::LocalUnconfined,
            EnforcementCompletenessV1::None,
        )
        .expect("local explicit unconfined ok");
        assert_eq!(receipt.enforcement, EnforcementCompletenessV1::None);
        assert!(
            receipt.access.effective.is_empty(),
            "effective must be empty (none)"
        );
    }

    #[test]
    fn r71_local_without_unconfined_policy_fails_closed() {
        let requested = BTreeSet::from([ResourceAccessV1::Read]);
        let error = verify_enforcement(
            &resource(),
            &requested,
            &AccessWideningPolicyV1::Exact,
            &requested,
            SandboxBackendClassV1::LocalUnconfined,
            EnforcementCompletenessV1::None,
        )
        .expect_err("must fail");
        assert!(matches!(
            error,
            EnforcementVerificationErrorV1::LocalRequiresUnconfined
        ));
    }
}
