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
