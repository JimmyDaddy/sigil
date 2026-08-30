//! RFC-0071 section 11.6: eager MCP/extension admission (current-schema, no fake continuity).
//!
//! Eager MCP stdio servers may start before any ordinary tool call, so they cannot fabricate a
//! ToolPermissionPlanV3. This module is the independent but isomorphic admission: durable config
//! grant only; Deny and AskUnsupported fail closed on every surface and never sign a token. V1
//! deliberately does not fake RFC-0060 interactive continuity.

use serde::{Deserialize, Serialize};

use crate::resource::{
    AuthorityGeneration, CanonicalHash, ExtensionKindV1, OpaqueAdmissionId, OpaqueDomainEventId,
    OpaqueExtensionGrantRef, OpaqueExtensionId, PhysicalAttemptId, RequestedEnforcementV1,
    ResourceJournalScopeV1, ResourceOwnerScopeV1, ResourceRequirementSetV1,
};

/// Durable admission scope: every managed execution selects exactly one durable domain writer.
pub fn extension_owner_scope(
    extension_kind: ExtensionKindV1,
    extension_id: OpaqueExtensionId,
    generation: u64,
) -> ResourceOwnerScopeV1 {
    ResourceOwnerScopeV1::ExtensionProcess {
        extension_kind,
        extension_id,
        generation,
    }
}

/// Closed restart policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtensionRestartPolicyV1 {
    Never,
    OnFailure,
    OnConfigChange,
}

/// Pathless extension execution plan (side-effect-free resolver output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionProcessPlanV1 {
    pub extension_kind: ExtensionKindV1,
    pub extension_id: OpaqueExtensionId,
    pub config_generation: u64,
    pub attempt_journal_scope: ResourceJournalScopeV1,
    pub attempt_journal_scope_hash: CanonicalHash,
    pub executable_and_args_digest: CanonicalHash,
    pub config_policy_digest: CanonicalHash,
    pub permission_upper_bound_hash: CanonicalHash,
    pub execution_plan_draft_hash: CanonicalHash,
    pub resource_plan_hash: CanonicalHash,
    pub requirement_set_hash: CanonicalHash,
    pub requested_enforcement_hash: CanonicalHash,
    pub resolver_proof_digest: CanonicalHash,
    pub sandbox_preview_hash: CanonicalHash,
    pub capture_policy_hash: CanonicalHash,
    pub resource_limits_hash: CanonicalHash,
    pub restart_policy: ExtensionRestartPolicyV1,
    pub extension_plan_hash: CanonicalHash,
}

/// Closed extension approval decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtensionApprovalDecisionV1 {
    AllowByDurableConfigGrant {
        grant_ref: OpaqueExtensionGrantRef,
        grant_hash: CanonicalHash,
    },
    Deny,
    AskUnsupported,
}

/// Durable admission scope: session log or application control log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DurableAdmissionScopeV1 {
    Session {
        session_log_id: String,
    },
    ApplicationControl {
        control_log_id: String,
        workspace_id: Option<String>,
    },
}

/// Extension process decision (durable facts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionProcessDecisionV1 {
    pub decision_id: String,
    pub durable_scope: DurableAdmissionScopeV1,
    pub domain_event_id: OpaqueDomainEventId,
    pub extension_plan_hash: CanonicalHash,
    pub attempt_journal_scope_hash: CanonicalHash,
    pub policy_version: String,
    pub authorization: ExtensionApprovalDecisionV1,
    pub decision_hash: CanonicalHash,
}

/// Extension process admission (all digests bound; one physical attempt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionProcessAdmissionV1 {
    pub admission_id: OpaqueAdmissionId,
    pub physical_attempt_id: PhysicalAttemptId,
    pub extension_kind: ExtensionKindV1,
    pub extension_id: OpaqueExtensionId,
    pub config_generation: u64,
    pub authority_generation: AuthorityGeneration,
    pub attempt_journal_scope: ResourceJournalScopeV1,
    pub attempt_journal_scope_hash: CanonicalHash,
    pub executable_and_args_digest: CanonicalHash,
    pub config_policy_digest: CanonicalHash,
    pub permission_upper_bound_hash: CanonicalHash,
    pub execution_plan_draft_hash: CanonicalHash,
    pub resource_plan_hash: CanonicalHash,
    pub extension_plan_hash: CanonicalHash,
    pub decision_hash: CanonicalHash,
    pub durable_scope_hash: CanonicalHash,
    pub extension_start_event_digest: CanonicalHash,
    pub resource_requirements: ResourceRequirementSetV1,
    pub requirement_set_hash: CanonicalHash,
    pub requested_enforcement: RequestedEnforcementV1,
    pub requested_enforcement_hash: CanonicalHash,
    pub resolver_proof_digest: CanonicalHash,
    pub sandbox_preview_hash: CanonicalHash,
    pub capture_policy_hash: CanonicalHash,
    pub resource_limits_hash: CanonicalHash,
    pub restart_policy: ExtensionRestartPolicyV1,
    pub admission_hash: CanonicalHash,
}

/// Extension execution admission token (one claim; non-clone).
#[derive(Debug)]
pub struct ExtensionExecutionAdmissionTokenV1 {
    admission: ExtensionProcessAdmissionV1,
    #[allow(dead_code)]
    claim: crate::managed_file_access::NonCloneOneShotClaim,
}

impl ExtensionExecutionAdmissionTokenV1 {
    pub fn admission(&self) -> &ExtensionProcessAdmissionV1 {
        &self.admission
    }
}

/// Closed extension admission error taxonomy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtensionAdmissionErrorV1 {
    #[error("no current-schema session or application control writer is available; failing closed")]
    DomainSinkUnavailable,
    #[error("AskUnsupported is not supported on any surface in V1")]
    AskUnsupported,
    #[error("durable config grant is stale, revoked or hash-drifted")]
    ConfigGrantDrift,
    #[error(
        "config, executable, requirement, enforcement or authority generation drifted; replan required"
    )]
    AdmissionDrift,
    #[error("active extension resource blocker is unresolved; no restart storm")]
    ActiveBlocker,
}

/// Validates that Deny / AskUnsupported never produce an admission.
pub fn authorize_extension(
    decision: &ExtensionProcessDecisionV1,
    plan: &ExtensionProcessPlanV1,
) -> Result<ExtensionApprovalDecisionV1, ExtensionAdmissionErrorV1> {
    match &decision.authorization {
        ExtensionApprovalDecisionV1::AllowByDurableConfigGrant { .. } => {
            if plan.extension_plan_hash != decision.extension_plan_hash {
                return Err(ExtensionAdmissionErrorV1::AdmissionDrift);
            }
            Ok(decision.authorization.clone())
        }
        ExtensionApprovalDecisionV1::Deny => Err(ExtensionAdmissionErrorV1::ConfigGrantDrift),
        ExtensionApprovalDecisionV1::AskUnsupported => {
            Err(ExtensionAdmissionErrorV1::AskUnsupported)
        }
    }
}

/// Checks the restart gate: an unresolved active blocker forbids restart storms.
pub fn check_restart_gate(active_blocker_resolved: bool) -> Result<(), ExtensionAdmissionErrorV1> {
    if !active_blocker_resolved {
        return Err(ExtensionAdmissionErrorV1::ActiveBlocker);
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/extension_admission_tests.rs"]
mod tests;
