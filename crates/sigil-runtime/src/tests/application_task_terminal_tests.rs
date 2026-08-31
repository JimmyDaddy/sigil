use anyhow::Result;
use sigil_kernel::{PublicRunEventKind, Session, TaskId, TaskRunStatus};

use super::{ApplicationRunTerminalStatus, task_control::application_task_continuation_terminal};

#[test]
fn task_continuation_terminal_preserves_each_noncompleted_status() -> Result<()> {
    let session = Session::new("provider", "model");
    let task_id = TaskId::new("task-terminal-status")?;
    let cases = [
        (
            TaskRunStatus::Cancelled,
            ApplicationRunTerminalStatus::Cancelled,
        ),
        (
            TaskRunStatus::Interrupted,
            ApplicationRunTerminalStatus::Interrupted,
        ),
        (TaskRunStatus::Paused, ApplicationRunTerminalStatus::Paused),
        (
            TaskRunStatus::Started,
            ApplicationRunTerminalStatus::Blocked,
        ),
        (
            TaskRunStatus::Running,
            ApplicationRunTerminalStatus::Blocked,
        ),
        (TaskRunStatus::Failed, ApplicationRunTerminalStatus::Failed),
    ];

    for (task_status, expected_terminal) in cases {
        let (terminal, final_answer, event) =
            application_task_continuation_terminal(&session, &task_id, task_status)?;
        assert_eq!(terminal, expected_terminal);
        assert!(final_answer.is_none());
        match (task_status, event) {
            (TaskRunStatus::Cancelled, PublicRunEventKind::RunCancelled)
            | (TaskRunStatus::Interrupted, PublicRunEventKind::RunInterrupted { .. })
            | (TaskRunStatus::Paused, PublicRunEventKind::RunPaused { .. })
            | (
                TaskRunStatus::Started | TaskRunStatus::Running,
                PublicRunEventKind::RunBlocked { .. },
            )
            | (TaskRunStatus::Failed, PublicRunEventKind::RunFailed { .. }) => {}
            (status, event) => panic!(
                "Task status {status:?} projected to an incompatible public terminal {event:?}"
            ),
        }
    }
    Ok(())
}
