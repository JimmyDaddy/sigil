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
mod tests {
    use super::*;
    use crate::resource::OpaqueWorkspaceId;

    fn hash(seed: u8) -> CanonicalHash {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        CanonicalHash::from_bytes(bytes)
    }

    fn plan() -> ExtensionProcessPlanV1 {
        ExtensionProcessPlanV1 {
            extension_kind: ExtensionKindV1::McpStdio,
            extension_id: OpaqueExtensionId::new("mcp-1".to_owned()),
            config_generation: 1,
            attempt_journal_scope: ResourceJournalScopeV1::Workspace(OpaqueWorkspaceId::new(
                "w1".to_owned(),
            )),
            attempt_journal_scope_hash: hash(1),
            executable_and_args_digest: hash(2),
            config_policy_digest: hash(3),
            permission_upper_bound_hash: hash(4),
            execution_plan_draft_hash: hash(5),
            resource_plan_hash: hash(6),
            requirement_set_hash: hash(7),
            requested_enforcement_hash: hash(8),
            resolver_proof_digest: hash(9),
            sandbox_preview_hash: hash(10),
            capture_policy_hash: hash(11),
            resource_limits_hash: hash(12),
            restart_policy: ExtensionRestartPolicyV1::OnFailure,
            extension_plan_hash: hash(13),
        }
    }

    fn decision(
        auth: ExtensionApprovalDecisionV1,
        plan_hash: CanonicalHash,
    ) -> ExtensionProcessDecisionV1 {
        ExtensionProcessDecisionV1 {
            decision_id: "decision-1".to_owned(),
            durable_scope: DurableAdmissionScopeV1::ApplicationControl {
                control_log_id: "control-1".to_owned(),
                workspace_id: Some("w1".to_owned()),
            },
            domain_event_id: OpaqueDomainEventId::new("event-1".to_owned()),
            extension_plan_hash: plan_hash,
            attempt_journal_scope_hash: hash(1),
            policy_version: "v1".to_owned(),
            authorization: auth,
            decision_hash: hash(14),
        }
    }

    #[test]
    fn r71_extension_allow_by_durable_grant_requires_exact_plan_hash() {
        let plan = plan();
        let decision = decision(
            ExtensionApprovalDecisionV1::AllowByDurableConfigGrant {
                grant_ref: OpaqueExtensionGrantRef::new("grant-1".to_owned()),
                grant_hash: hash(15),
            },
            plan.extension_plan_hash,
        );
        let auth = authorize_extension(&decision, &plan).expect("approved");
        assert!(matches!(
            auth,
            ExtensionApprovalDecisionV1::AllowByDurableConfigGrant { .. }
        ));
    }

    #[test]
    fn r71_extension_deny_fails_closed() {
        let plan = plan();
        let decision = decision(ExtensionApprovalDecisionV1::Deny, plan.extension_plan_hash);
        let error = authorize_extension(&decision, &plan).expect_err("deny must fail");
        assert!(matches!(error, ExtensionAdmissionErrorV1::ConfigGrantDrift));
    }

    #[test]
    fn r71_extension_ask_unsupported_never_signs_token() {
        let plan = plan();
        let decision = decision(
            ExtensionApprovalDecisionV1::AskUnsupported,
            plan.extension_plan_hash,
        );
        let error = authorize_extension(&decision, &plan).expect_err("unsupported");
        assert!(matches!(error, ExtensionAdmissionErrorV1::AskUnsupported));
    }

    #[test]
    fn r71_extension_plan_drift_replans_instead_of_signing() {
        let plan = plan();
        let mut plan = plan;
        plan.extension_plan_hash = hash(99);
        let decision = decision(
            ExtensionApprovalDecisionV1::AllowByDurableConfigGrant {
                grant_ref: OpaqueExtensionGrantRef::new("grant-1".to_owned()),
                grant_hash: hash(15),
            },
            hash(13),
        );
        let error = authorize_extension(&decision, &plan).expect_err("drift");
        assert!(matches!(error, ExtensionAdmissionErrorV1::AdmissionDrift));
    }

    #[test]
    fn r71_extension_active_blocker_gate() {
        let blocker = check_restart_gate(false).expect_err("blocked");
        assert!(matches!(blocker, ExtensionAdmissionErrorV1::ActiveBlocker));
        check_restart_gate(true).expect("ok");
    }
}
