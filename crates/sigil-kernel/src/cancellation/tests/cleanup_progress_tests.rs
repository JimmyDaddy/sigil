use super::*;
use crate::RunCancellationOwner;

#[test]
fn cleanup_progress_tracks_real_scopes_without_granting_terminal_authority() {
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let first = handle.begin_cleanup_stage(RunCleanupStage::ResourceSettlement);
    let second = handle.begin_cleanup_stage(RunCleanupStage::ResourceSettlement);
    let before = owner.stop_capability().cleanup_progress();
    assert_eq!(before.len(), 6);
    assert_eq!(before[3].active, 2);
    first.finish(true);
    second.finish(false);
    drop(handle.begin_cleanup_stage(RunCleanupStage::OutputDrain));
    let after = handle.cleanup_progress();
    assert_eq!(after[3].active, 0);
    assert_eq!(after[3].completed, 1);
    assert_eq!(after[3].failed, 1);
    assert_eq!(after[2].abandoned, 1);
    assert!(owner.is_quiescent());
    assert!(
        owner.cleanup_complete(),
        "diagnostics cannot change real cleanup authority"
    );
    handle.mark_cleanup_incomplete();
    handle
        .begin_cleanup_stage(RunCleanupStage::ResourceSettlement)
        .finish(true);
    assert!(
        !owner.cleanup_complete(),
        "later observations cannot erase a real failure"
    );
}
