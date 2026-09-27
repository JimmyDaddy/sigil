use super::*;

#[test]
fn task_verification_feedback_binding_requires_exact_bounded_current_evidence() -> Result<()> {
    let mut feedback = TaskVerificationFeedbackV1 {
        feedback_id: String::new(),
        task_id: TaskId::new("task-feedback")?,
        admission_id: "admission".to_owned(),
        attempt_id: "attempt".to_owned(),
        receipt_ids: vec!["receipt".to_owned()],
        policy_hash: "policy".to_owned(),
        workspace_snapshot_id: "snapshot".to_owned(),
        status: TaskVerificationFeedbackStatusV1::Pending,
        reason: None,
    };
    feedback.feedback_id = feedback.expected_id();
    feedback.validate()?;
    for field in [
        "task_id",
        "admission_id",
        "attempt_id",
        "receipt_ids",
        "policy_hash",
        "workspace_snapshot_id",
    ] {
        let mut value = serde_json::to_value(&feedback)?;
        value[field] = if field == "receipt_ids" {
            serde_json::json!(["other"])
        } else {
            serde_json::json!("other")
        };
        assert!(
            serde_json::from_value::<TaskVerificationFeedbackV1>(value)?
                .validate()
                .is_err(),
            "{field} cannot change behind the feedback identity"
        );
    }
    let mut duplicate = feedback.clone();
    duplicate.receipt_ids.push("receipt".to_owned());
    duplicate.feedback_id = duplicate.expected_id();
    assert!(duplicate.validate().is_err());
    let mut selected = feedback;
    selected.status = TaskVerificationFeedbackStatusV1::Repair;
    selected.validate()?;
    selected.reason = Some("x".repeat(1025));
    assert!(selected.validate().is_err());
    Ok(())
}
