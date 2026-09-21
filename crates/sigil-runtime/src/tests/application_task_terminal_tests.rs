use anyhow::Result;
use sigil_kernel::{
    ControlEntry, JsonlSessionStore, PublicEventOutboxProjectionV1, PublicRunEventKind, Session,
    SessionRef, TaskId, TaskRunEntry, TaskRunStatus,
};

use super::{ApplicationRunTerminalStatus, task_control::application_task_continuation_terminal};

#[test]
fn task_failure_recorder_commits_real_reason_as_failed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("provider", "model").with_store(store.clone());
    let task_id = TaskId::new("task-recorder-failure")?;
    let reason = "missing terminal task field schema_version";
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        objective: "read output".to_owned(),
        title: None,
        status: TaskRunStatus::Failed,
        reason: Some(reason.to_owned()),
    }))?;
    let recorder = crate::ApplicationRunEventRecorder::start(&session, "run-failure", "read")?;
    recorder.finish_task(&session, &task_id, TaskRunStatus::Failed)?;
    let outbox = PublicEventOutboxProjectionV1::from_records(
        &JsonlSessionStore::read_event_records(temp.path().join("session.jsonl"))?,
    )?;
    assert!(outbox.events_in_order().iter().any(|entry| {
        matches!(&entry.event.event, PublicRunEventKind::RunFailed { error } if error == reason)
    }));
    assert!(
        !outbox
            .events_in_order()
            .iter()
            .any(|entry| { matches!(&entry.event.event, PublicRunEventKind::RunBlocked { .. }) })
    );
    Ok(())
}

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
