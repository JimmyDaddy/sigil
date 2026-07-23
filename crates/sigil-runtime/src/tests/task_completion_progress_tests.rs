use anyhow::Result;
use sigil_kernel::{TaskId, TaskStepId};

use super::{
    TaskCompletionOutcome, TaskCompletionProgressRegistration, TaskCompletionProgressRegistry,
};

fn registration(step_id: &str, title: &str) -> Result<TaskCompletionProgressRegistration> {
    Ok(TaskCompletionProgressRegistration {
        step_id: TaskStepId::new(step_id)?,
        title: title.to_owned(),
    })
}

#[test]
fn snapshot_keeps_arrival_order_separate_from_request_order() -> Result<()> {
    let registry = TaskCompletionProgressRegistry::default();
    let generation = registry.begin(
        &TaskId::new("task_1")?,
        3,
        vec![
            registration("read_a", "Read A")?,
            registration("read_b", "Read B")?,
        ],
    );

    registry.record_arrival(generation, 1, 0, TaskCompletionOutcome::Succeeded);
    registry.record_arrival(generation, 0, 1, TaskCompletionOutcome::Failed);

    let snapshot = registry.snapshot();
    let batch = snapshot.batch.expect("active completion batch");
    assert_eq!(batch.task_id, "task_1");
    assert_eq!(batch.plan_version, 3);
    assert_eq!(batch.arrived, 2);
    assert_eq!(batch.total, 2);
    assert_eq!(batch.members[0].request_order, 1);
    assert_eq!(batch.members[0].arrival_order, Some(2));
    assert_eq!(
        batch.members[0].outcome,
        Some(TaskCompletionOutcome::Failed)
    );
    assert_eq!(batch.members[1].request_order, 2);
    assert_eq!(batch.members[1].arrival_order, Some(1));
    assert_eq!(
        batch.members[1].outcome,
        Some(TaskCompletionOutcome::Succeeded)
    );
    Ok(())
}

#[test]
fn stale_and_duplicate_arrivals_do_not_corrupt_the_current_batch() -> Result<()> {
    let registry = TaskCompletionProgressRegistry::default();
    let stale_generation = registry.begin(
        &TaskId::new("task_1")?,
        1,
        vec![registration("old", "Old")?],
    );
    let current_generation = registry.begin(
        &TaskId::new("task_2")?,
        2,
        vec![registration("new", "New")?],
    );

    registry.record_arrival(stale_generation, 0, 0, TaskCompletionOutcome::Failed);
    registry.record_arrival(current_generation, 0, 0, TaskCompletionOutcome::Succeeded);
    registry.record_arrival(current_generation, 0, 1, TaskCompletionOutcome::Failed);

    let snapshot = registry.snapshot();
    let batch = snapshot.batch.expect("current completion batch");
    assert_eq!(batch.task_id, "task_2");
    assert_eq!(batch.arrived, 1);
    assert_eq!(batch.members[0].arrival_order, Some(1));
    assert_eq!(
        batch.members[0].outcome,
        Some(TaskCompletionOutcome::Succeeded)
    );
    Ok(())
}
