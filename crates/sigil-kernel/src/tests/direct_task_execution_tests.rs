use anyhow::Result;

use crate::{
    PlanId, TaskDirectExecutionAdmittedV1, TaskDirectExecutionAttemptV1, TaskId,
    TaskParticipantAttemptStatus,
};

#[test]
fn approved_plan_direct_admission_is_objective_bound_and_deterministic() -> Result<()> {
    let task_id = TaskId::new("task-direct")?;
    let plan_id = PlanId::new("plan-direct")?;
    let first = TaskDirectExecutionAdmittedV1::approved_plan(
        task_id.clone(),
        "Implement the approved plan",
        plan_id.clone(),
        format!("sha256:{}", "a".repeat(64)),
        10,
    );
    let second = TaskDirectExecutionAdmittedV1::approved_plan(
        task_id,
        "Implement the approved plan",
        plan_id,
        format!("sha256:{}", "a".repeat(64)),
        99,
    );

    first.validate()?;
    assert_eq!(first.admission_id, second.admission_id);
    assert!(first.matches_objective("Implement the approved plan"));
    assert!(!first.matches_objective("Another objective"));
    Ok(())
}

#[test]
fn direct_attempt_has_no_task_plan_or_step_identity() -> Result<()> {
    let admission = TaskDirectExecutionAdmittedV1::planner_fallback(
        TaskId::new("task-fallback")?,
        "Do the work",
        "planner-attempt-1",
        20,
    );
    let mut attempt = TaskDirectExecutionAttemptV1::started(&admission, 1);
    attempt.validate()?;
    attempt.status = TaskParticipantAttemptStatus::Completed;
    attempt.reason = Some("done".to_owned());
    attempt.final_message_id = Some("message-direct-final".to_owned());
    attempt.output_hash = Some(format!("sha256:{}", "a".repeat(64)));
    attempt.validate()?;
    Ok(())
}
