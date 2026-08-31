use super::*;

#[test]
fn eval_projection_preserves_failure_cancel_pause_and_interrupt() {
    for (terminal, expected) in [
        (
            ApplicationRunTerminalStatus::Succeeded,
            RunStatus::Completed,
        ),
        (ApplicationRunTerminalStatus::Failed, RunStatus::Failed),
        (ApplicationRunTerminalStatus::Blocked, RunStatus::Blocked),
        (
            ApplicationRunTerminalStatus::Cancelled,
            RunStatus::Cancelled,
        ),
        (
            ApplicationRunTerminalStatus::Interrupted,
            RunStatus::Interrupted,
        ),
        (ApplicationRunTerminalStatus::Paused, RunStatus::Paused),
        (
            ApplicationRunTerminalStatus::AwaitingUserInput,
            RunStatus::Paused,
        ),
    ] {
        assert_eq!(terminal_run_status(terminal), expected);
    }
}
