use super::*;

#[test]
fn stale_verification_without_action_hints_cannot_complete_direct_task() {
    let mut evaluation = crate::evaluate_readiness(&crate::ReadinessInput::new_run(
        crate::RunStatus::Completed,
        crate::VerificationPolicy::no_checks_required("scope"),
    ));
    assert!(evaluation.required_actions.is_empty());
    let mut readiness = ReadinessEvaluatedEntry {
        scope: EvidenceScope::Task("task".to_owned()),
        evaluation: evaluation.clone(),
        policy_hash: None,
        workspace_snapshot_id: None,
    };
    assert!(
        !readiness_blocks_task(&readiness),
        "no-check tasks retain their minimal path"
    );
    evaluation.verification_verdict = crate::VerificationVerdict::Stale;
    readiness.evaluation = evaluation;
    assert!(
        readiness_blocks_task(&readiness),
        "action hints are not verification authority"
    );
}
