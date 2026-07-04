use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde_json::json;

use super::{
    TerminalExecutionBackendCapabilities, TerminalExecutionBackendKind, TerminalTaskEntry,
    TerminalTaskHandle, TerminalTaskId, TerminalTaskProjection, TerminalTaskStatus,
    terminal_cleanup_receipt_for_status,
};
use crate::{
    ControlEntry, ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionCleanupStatus,
    ExecutionSandboxProfile, SessionLogEntry,
};

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
    assert_eq!(
        TerminalExecutionBackendKind::LocalProcess.as_str(),
        "local_process"
    );
    assert_eq!(TerminalExecutionBackendKind::LocalPty.as_str(), "local_pty");
    assert_eq!(
        TerminalExecutionBackendKind::SandboxedPty.as_str(),
        "sandboxed_pty"
    );
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

#[test]
fn terminal_task_entry_projects_from_terminal_tool_details() -> Result<()> {
    let details = json!({
        "task_id": "terminal-1",
        "status": "cancelled",
        "status_detail": { "state": "cancelled" },
        "command": "cargo test -- --ignored",
        "cwd": ".",
        "shell": "zsh",
        "log_path": ".sigil/terminal/terminal-1/output.log",
        "created_at_ms": 100,
        "execution_backend": "local_pty",
        "execution_backend_capabilities": {
            "persistent_pty": true,
            "input": true,
            "resize": true,
            "cancel": true,
            "output_log": true
        },
        "enforcement_backend": "local",
        "enforcement_backend_capabilities": {
            "filesystem_isolation": false,
            "network_isolation": false,
            "process_isolation": false,
            "resource_limits": false,
            "persistent_pty": false,
            "workspace_snapshot": false
        },
        "sandbox_profile": "unconfined",
        "cleanup": {
            "status": "completed",
            "reason": "terminal process was cancelled and reaped"
        },
        "updated_at_ms": 140,
        "output_preview": "final tail",
        "output_hash": "sha256:def",
        "output_truncated": true
    });

    let entry = TerminalTaskEntry::from_tool_result_details(&details)?
        .expect("terminal metadata should project to an entry");

    assert_eq!(entry.handle.task_id.as_str(), "terminal-1");
    assert_eq!(entry.handle.command, "cargo test -- --ignored");
    assert_eq!(
        entry.handle.execution_backend,
        Some(TerminalExecutionBackendKind::LocalPty)
    );
    assert_eq!(
        entry.handle.execution_backend_capabilities,
        Some(TerminalExecutionBackendCapabilities::local_pty())
    );
    assert_eq!(
        entry.handle.enforcement_backend,
        Some(ExecutionBackendKind::Local)
    );
    assert_eq!(
        entry.handle.enforcement_backend_capabilities,
        Some(ExecutionBackendCapabilities::default())
    );
    assert_eq!(
        entry.handle.sandbox_profile,
        Some(ExecutionSandboxProfile::Unconfined)
    );
    assert!(matches!(entry.status, TerminalTaskStatus::Cancelled));
    assert_eq!(
        entry.cleanup.as_ref().expect("cleanup should parse").status,
        ExecutionCleanupStatus::Completed
    );
    assert_eq!(entry.output_preview.as_deref(), Some("final tail"));
    assert_eq!(entry.output_hash.as_deref(), Some("sha256:def"));
    assert!(entry.output_truncated);
    assert_eq!(entry.updated_at_ms, 140);
    Ok(())
}

#[test]
fn terminal_task_entry_ignores_non_terminal_tool_details_and_rejects_partial_metadata() {
    assert!(
        TerminalTaskEntry::from_tool_result_details(&json!({"task_id": "terminal-1"}))
            .expect("non-terminal metadata should not fail")
            .is_none()
    );
    assert!(
        TerminalTaskEntry::from_tool_result_details(&json!({
            "task_id": "terminal-1",
            "status_detail": { "state": "running" }
        }))
        .is_err()
    );
    assert!(
        TerminalTaskEntry::from_tool_result_details(&json!({
            "task_id": "terminal-1",
            "status_detail": { "state": "running" },
            "command": "cargo test",
            "cwd": ".",
            "shell": "zsh",
            "log_path": ".sigil/terminal/terminal-1/output.log",
            "created_at_ms": 100,
            "updated_at_ms": 120,
            "execution_backend_capabilities": "invalid"
        }))
        .is_err()
    );
}

#[test]
fn terminal_cleanup_receipt_maps_terminal_statuses_without_claiming_running_cleanup() {
    assert!(terminal_cleanup_receipt_for_status(&TerminalTaskStatus::Running).is_none());
    assert_eq!(
        terminal_cleanup_receipt_for_status(&TerminalTaskStatus::Exited { exit_code: Some(0) })
            .expect("exited status should have cleanup receipt")
            .status,
        ExecutionCleanupStatus::NotNeeded
    );
    assert_eq!(
        terminal_cleanup_receipt_for_status(&TerminalTaskStatus::Cancelled)
            .expect("cancelled status should have cleanup receipt")
            .status,
        ExecutionCleanupStatus::Completed
    );
    assert_eq!(
        terminal_cleanup_receipt_for_status(&TerminalTaskStatus::Failed {
            reason: "failed to kill terminal process".to_owned(),
        })
        .expect("failed cleanup should be recorded")
        .status,
        ExecutionCleanupStatus::Failed
    );
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
            execution_backend: None,
            execution_backend_capabilities: None,
            enforcement_backend: None,
            enforcement_backend_capabilities: None,
            sandbox_profile: None,
        },
        status,
        output_preview: Some("tail".to_owned()),
        output_hash: Some("sha256:abc".to_owned()),
        output_truncated: true,
        cleanup: None,
        updated_at_ms,
    }
}
