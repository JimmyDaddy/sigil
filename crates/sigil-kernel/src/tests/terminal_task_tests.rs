use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::Result;

use super::{
    TerminalTaskEntry, TerminalTaskHandle, TerminalTaskId, TerminalTaskProjection,
    TerminalTaskStatus,
};
use crate::{ControlEntry, SessionLogEntry};

#[test]
fn terminal_task_id_accepts_stable_values_and_rejects_path_unsafe_values() {
    assert_eq!(
        TerminalTaskId::new("terminal-1")
            .expect("valid id")
            .as_str(),
        "terminal-1"
    );
    assert!(TerminalTaskId::new("").is_err());
    assert!(TerminalTaskId::new("..").is_err());
    assert!(TerminalTaskId::new("dir/task").is_err());
    assert!(TerminalTaskId::new("terminal 1").is_err());
    assert!(serde_json::from_str::<TerminalTaskId>(r#""dir/task""#).is_err());
}

#[test]
fn terminal_task_control_entry_roundtrips_with_snake_case_payload() -> Result<()> {
    let entry = SessionLogEntry::Control(ControlEntry::TerminalTask(sample_entry(
        TerminalTaskStatus::Running,
        110,
    )));

    let json = serde_json::to_string(&entry)?;
    let restored: SessionLogEntry = serde_json::from_str(&json)?;

    assert!(json.contains("terminal_task"));
    assert!(json.contains("task_id"));
    assert!(json.contains("\"state\":\"running\""));
    assert!(json.contains("output_truncated"));
    assert!(matches!(
        restored,
        SessionLogEntry::Control(ControlEntry::TerminalTask(entry))
            if entry.handle.task_id.as_str() == "terminal-1"
                && matches!(entry.status, TerminalTaskStatus::Running)
                && entry.output_truncated
    ));
    Ok(())
}

#[test]
fn terminal_task_control_entry_accepts_legacy_pascal_case_alias() -> Result<()> {
    let json = r#"{"control":{"TerminalTask":{"handle":{"task_id":"terminal-1","command":"cargo test","cwd":".","shell":"zsh","log_path":".sigil/terminal/terminal-1/output.log","created_at_ms":100},"status":{"state":"exited","exit_code":0},"output_preview":"ok","output_hash":"sha256:abc","output_truncated":false,"updated_at_ms":120}}}"#;

    let restored: SessionLogEntry = serde_json::from_str(json)?;

    assert!(matches!(
        restored,
        SessionLogEntry::Control(ControlEntry::TerminalTask(entry))
            if entry.handle.task_id.as_str() == "terminal-1"
                && matches!(entry.status, TerminalTaskStatus::Exited { exit_code: Some(0) })
                && entry.updated_at_ms == 120
    ));
    Ok(())
}

#[test]
fn terminal_task_projection_replays_latest_status_and_active_tasks() {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TerminalTask(sample_entry(
            TerminalTaskStatus::Starting,
            100,
        ))),
        SessionLogEntry::Control(ControlEntry::TerminalTask(sample_entry(
            TerminalTaskStatus::Running,
            110,
        ))),
        SessionLogEntry::Control(ControlEntry::TerminalTask(sample_entry(
            TerminalTaskStatus::Exited { exit_code: Some(0) },
            140,
        ))),
    ];

    let projection = TerminalTaskProjection::from_entries(&entries);
    let latest = projection.latest().expect("latest terminal task");

    assert_eq!(
        projection.replay_order,
        vec![terminal_task_id(), terminal_task_id(), terminal_task_id()]
    );
    assert_eq!(
        projection.latest_task_id.as_ref(),
        Some(&terminal_task_id())
    );
    assert!(projection.active_task_ids.is_empty());
    assert!(matches!(
        latest.status,
        TerminalTaskStatus::Exited { exit_code: Some(0) }
    ));
}

#[test]
fn terminal_task_projection_keeps_multiple_active_tasks_sorted_by_id() {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TerminalTask(sample_entry_for_id(
            "terminal-b",
            TerminalTaskStatus::Running,
            100,
        ))),
        SessionLogEntry::Control(ControlEntry::TerminalTask(sample_entry_for_id(
            "terminal-a",
            TerminalTaskStatus::Starting,
            110,
        ))),
    ];

    let projection = TerminalTaskProjection::from_entries(&entries);

    assert_eq!(
        projection.active_task_ids,
        vec![
            TerminalTaskId::new("terminal-a").expect("valid task id"),
            TerminalTaskId::new("terminal-b").expect("valid task id"),
        ]
    );
}

#[test]
fn terminal_task_projection_builds_interrupted_entries_for_missing_running_tasks() {
    let entries = vec![SessionLogEntry::Control(ControlEntry::TerminalTask(
        sample_entry(TerminalTaskStatus::Running, 110),
    ))];
    let projection = TerminalTaskProjection::from_entries(&entries);

    let interrupted =
        projection.interrupted_entries_for_missing_processes(&BTreeSet::new(), 200, 60);

    assert_eq!(interrupted.len(), 1);
    assert!(matches!(
        interrupted[0].status,
        TerminalTaskStatus::Interrupted
    ));
    assert_eq!(interrupted[0].updated_at_ms, 200);
    assert_eq!(interrupted[0].output_preview.as_deref(), Some("tail"));
}

#[test]
fn terminal_task_projection_respects_live_processes_and_starting_timeout() {
    let recent_starting = sample_entry_for_id("terminal-recent", TerminalTaskStatus::Starting, 170);
    let stale_starting = sample_entry_for_id("terminal-stale", TerminalTaskStatus::Starting, 100);
    let running_live = sample_entry_for_id("terminal-live", TerminalTaskStatus::Running, 120);
    let projection = TerminalTaskProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::TerminalTask(recent_starting)),
        SessionLogEntry::Control(ControlEntry::TerminalTask(stale_starting)),
        SessionLogEntry::Control(ControlEntry::TerminalTask(running_live)),
    ]);
    let live_ids = BTreeSet::from([TerminalTaskId::new("terminal-live").expect("valid task id")]);

    let interrupted = projection.interrupted_entries_for_missing_processes(&live_ids, 200, 60);

    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].handle.task_id.as_str(), "terminal-stale");
}

#[test]
fn terminal_task_status_labels_and_terminal_state_are_stable() {
    assert_eq!(TerminalTaskStatus::Starting.as_str(), "starting");
    assert_eq!(TerminalTaskStatus::Running.as_str(), "running");
    assert_eq!(
        TerminalTaskStatus::Exited { exit_code: Some(0) }.as_str(),
        "exited"
    );
    assert_eq!(
        TerminalTaskStatus::Failed {
            reason: "spawn failed".to_owned()
        }
        .as_str(),
        "failed"
    );
    assert_eq!(TerminalTaskStatus::Cancelled.as_str(), "cancelled");
    assert_eq!(TerminalTaskStatus::Interrupted.as_str(), "interrupted");
    assert!(TerminalTaskStatus::Running.is_active());
    assert!(TerminalTaskStatus::Exited { exit_code: None }.is_terminal());
}

fn terminal_task_id() -> TerminalTaskId {
    TerminalTaskId::new("terminal-1").expect("valid terminal task id")
}

fn sample_entry(status: TerminalTaskStatus, updated_at_ms: u64) -> TerminalTaskEntry {
    sample_entry_for_id("terminal-1", status, updated_at_ms)
}

fn sample_entry_for_id(
    task_id: &str,
    status: TerminalTaskStatus,
    updated_at_ms: u64,
) -> TerminalTaskEntry {
    TerminalTaskEntry {
        handle: TerminalTaskHandle {
            task_id: TerminalTaskId::new(task_id).expect("valid terminal task id"),
            command: "cargo test".to_owned(),
            cwd: PathBuf::from("."),
            shell: "zsh".to_owned(),
            log_path: Path::new(".sigil")
                .join("terminal")
                .join(task_id)
                .join("output.log"),
            created_at_ms: 100,
        },
        status,
        output_preview: Some("tail".to_owned()),
        output_hash: Some("sha256:abc".to_owned()),
        output_truncated: true,
        updated_at_ms,
    }
}
