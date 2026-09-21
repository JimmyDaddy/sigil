use anyhow::Result;

use crate::{
    PlanId, TaskDirectExecutionAdmittedV1, TaskDirectExecutionAttemptV1,
    TaskExecutionAttemptStatus, TaskId,
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
    let admission = TaskDirectExecutionAdmittedV1::task_request(
        TaskId::new("task-fallback")?,
        "Do the work",
        20,
    );
    let mut attempt = TaskDirectExecutionAttemptV1::started(&admission, 1);
    attempt.validate()?;
    attempt.status = TaskExecutionAttemptStatus::Completed;
    attempt.reason = Some("done".to_owned());
    attempt.final_message_id = Some("message-direct-final".to_owned());
    attempt.output_hash = Some(format!("sha256:{}", "a".repeat(64)));
    attempt.validate()?;
    Ok(())
}

#[test]
fn task_request_direct_admission_has_no_planner_identity() -> Result<()> {
    let admission = TaskDirectExecutionAdmittedV1::task_request(
        TaskId::new("task-model-owned")?,
        "Decide and execute the requested change",
        20,
    );
    admission.validate()?;
    assert!(matches!(
        admission.source,
        crate::TaskDirectExecutionSourceV1::TaskRequest
    ));
    assert!(admission.matches_objective("Decide and execute the requested change"));
    Ok(())
}
