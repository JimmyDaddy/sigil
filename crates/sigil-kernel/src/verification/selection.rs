//! The canonical distinction between a runnable catalog and required verification.

use super::*;

/// Source identity for one required check in the same canonical policy record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum VerificationRequirementSourceV1 {
    StepContract {
        check_spec_id: String,
        task_id: crate::TaskId,
        plan_version: u32,
        step_id: crate::TaskStepId,
        index: u32,
        contract_set_sha256: String,
    },
    ExistingPolicy {
        check_spec_id: String,
        source_event_id: String,
        policy_hash: String,
    },
}

impl VerificationRequirementSourceV1 {
    pub fn check_spec_id(&self) -> &str {
        match self {
            Self::StepContract { check_spec_id, .. }
            | Self::ExistingPolicy { check_spec_id, .. } => check_spec_id,
        }
    }
}

impl VerificationStateProjection {
    /// Resolves explicit policies in broad-to-narrow scope order. Catalog entries never become
    /// required merely because they exist, and narrower policy cannot remove broader checks.
    pub fn selected_policy(
        &self,
        scopes: &[EvidenceScope],
        scope_hash: &str,
    ) -> Result<VerificationPolicy> {
        let mut selected: Option<VerificationPolicy> = None;
        for scope in scopes {
            if let Some(entry) = self.latest_policy(scope) {
                selected = Some(match selected {
                    Some(parent) => parent.merge_child(&entry.policy)?,
                    None => entry.policy.clone(),
                });
            }
        }
        Ok(selected.unwrap_or_else(|| VerificationPolicy::no_checks_required(scope_hash)))
    }
}

/// An accepted requirement cannot be resolved to a real runnable CheckSpec. It remains a
/// resumable preflight blocker, never an implicit zero-check exemption.
#[derive(Debug, thiserror::Error)]
#[error("required verification check is unresolved: {check_spec_id}")]
pub struct UnresolvedVerificationRequirement {
    pub check_spec_id: String,
}
