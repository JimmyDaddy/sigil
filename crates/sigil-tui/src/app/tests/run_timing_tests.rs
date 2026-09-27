use super::*;

#[test]
fn feedback_and_admission_are_one_shot_without_becoming_run_authority() {
    let mut timing = SubmissionTiming::new();
    let local_id = timing.id.clone();
    timing.feedback_presented();
    timing.feedback_presented();
    timing.observe_run("observed-run");
    timing.observe_run("later-task-handoff");
    assert!(timing.feedback_recorded);
    assert!(timing.linked);
    assert_ne!(timing.id, local_id);
    assert_eq!(timing.id, "observed-run");
    let next = SubmissionTiming::new();
    assert!(!next.linked);
    assert!(!next.feedback_recorded);
}
