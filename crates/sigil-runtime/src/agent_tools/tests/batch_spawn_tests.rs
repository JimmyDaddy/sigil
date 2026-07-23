use super::*;

#[test]
fn batch_identity_is_stable_within_one_root_run_and_scoped_between_runs() -> Result<()> {
    let first = agent_batch_id("root-run-a", "call-1")?;
    let replay = agent_batch_id("root-run-a", "call-1")?;
    let other_run = agent_batch_id("root-run-b", "call-1")?;

    assert_eq!(first, replay);
    assert_ne!(first, other_run);
    Ok(())
}

#[test]
fn batch_identity_rejects_an_empty_root_run() {
    assert!(agent_batch_id(" ", "call-1").is_err());
}
