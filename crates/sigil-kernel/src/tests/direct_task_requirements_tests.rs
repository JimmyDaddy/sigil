use crate::*;
use anyhow::Result;

fn historical_baseline() -> Result<TaskDirectRequirementsBoundV1> {
    let mut baseline = TaskDirectRequirementsBoundV1 {
        schema_version: 1,
        task_id: TaskId::new("direct-requirements")?,
        admission_id: "admission-1".to_owned(),
        objective_hash: format!("sha256:{}", "a".repeat(64)),
        requirements: vec![DirectTaskRequirementV1 {
            start_byte: 0,
            end_byte: 12,
            interpretation: "Implement the requested change".to_owned(),
            required: true,
        }],
        baseline_digest: String::new(),
    };
    baseline.baseline_digest = baseline.digest()?;
    Ok(baseline)
}

#[test]
fn historical_direct_baseline_reopens_without_reinterpretation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("direct.jsonl"))?;
    let mut session = Session::load_from_store("fixture", "model", store.clone())?;
    let baseline = historical_baseline()?;
    session.append_control(session::ControlEntry::TaskDirectRequirementsBoundV1(
        baseline.clone(),
    ))?;
    drop(session);
    let reopened = Session::load_from_store("fixture", "model", store)?;
    assert!(reopened.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::Control(session::ControlEntry::TaskDirectRequirementsBoundV1(saved))
        if saved == &baseline
    )));
    Ok(())
}

#[test]
fn historical_direct_baseline_rejects_digest_and_record_corruption() -> Result<()> {
    let baseline = historical_baseline()?;
    baseline.validate()?;
    let mut changed = baseline.clone();
    changed.requirements[0].interpretation = "Different content".to_owned();
    assert!(changed.validate().is_err());
    for (start, end, interpretation) in [(12, 12, "valid"), (0, 12, ""), (0, 12, " ")] {
        let mut changed = baseline.clone();
        changed.requirements[0].start_byte = start;
        changed.requirements[0].end_byte = end;
        changed.requirements[0].interpretation = interpretation.to_owned();
        changed.baseline_digest = changed.digest()?;
        assert!(changed.validate().is_err());
    }
    let mut changed = serde_json::to_value(baseline)?;
    changed["unexpected"] = serde_json::json!(true);
    serde_json::from_value::<TaskDirectRequirementsBoundV1>(changed)?;
    Ok(())
}
