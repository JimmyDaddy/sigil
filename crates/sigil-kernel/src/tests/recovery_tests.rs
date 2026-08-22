use anyhow::Result;

use crate::{
    EffectSettlementV1, FailureScopeV1, RecoverabilityV1, RecoveryActionV1, RecoveryBlockerV1,
    RecoveryDomainV1, TaskId, TaskStepId,
};

fn blocker() -> RecoveryBlockerV1 {
    RecoveryBlockerV1 {
        schema_version: crate::RECOVERY_BLOCKER_SCHEMA_VERSION,
        blocker_id: "blocker-1".to_owned(),
        domain: RecoveryDomainV1::WorkspaceMutation,
        scope: FailureScopeV1::ToolEffect {
            task_id: Some(TaskId::new("task-1").expect("task id")),
            step_id: Some(TaskStepId::new("step-1").expect("step id")),
            effect_id: "effect-1".to_owned(),
        },
        recoverability: RecoverabilityV1::RebaseWorkspace,
        settlement: EffectSettlementV1::ConfirmedNoEffect,
        reason_code: "prepared_mutation_stale".to_owned(),
        safe_summary: "The prepared change is stale and needs a fresh workspace read.".to_owned(),
        evidence_digest: format!("sha256:{}", "a".repeat(64)),
        effect_id: Some("effect-1".to_owned()),
        available_actions: vec![RecoveryActionV1::RebaseWorkspace, RecoveryActionV1::Cancel],
        created_at_ms: 1,
    }
}

#[test]
fn recovery_blocker_validates_exact_effect_scope_and_safe_public_view() -> Result<()> {
    let blocker = blocker();
    blocker.validate()?;

    let public = blocker.public_view();
    assert_eq!(public.blocker_id, "blocker-1");
    assert_eq!(public.reason_code, "prepared_mutation_stale");
    assert_eq!(public.available_actions, blocker.available_actions);
    Ok(())
}

#[test]
fn recovery_blocker_rejects_mismatched_effect_and_duplicate_actions() {
    let mut invalid = blocker();
    invalid.effect_id = Some("other-effect".to_owned());
    assert!(invalid.validate().is_err());

    let mut invalid = blocker();
    invalid.available_actions.push(RecoveryActionV1::Cancel);
    assert!(invalid.validate().is_err());
}

#[test]
fn recovery_blocker_rejects_newer_schema_and_unsafe_summary() {
    let mut invalid = blocker();
    invalid.schema_version = crate::RECOVERY_BLOCKER_SCHEMA_VERSION.saturating_add(1);
    assert!(invalid.validate().is_err());

    let mut invalid = blocker();
    invalid.safe_summary = "x".repeat(1_025);
    assert!(invalid.validate().is_err());
}
