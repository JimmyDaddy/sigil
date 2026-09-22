//! Terminal task projection tests for the tool audit layer.

use super::terminal_task_latest_observation;
use crate::{
    TerminalReadinessStatus, TerminalTaskHandle, TerminalTaskId, TerminalTaskStatus,
    TerminalTaskSummary,
};

#[test]
fn stale_terminal_observation_exposes_bounded_latest_state_and_wait_action() {
    let summary = TerminalTaskSummary {
        schema_version: crate::TERMINAL_TASK_SCHEMA_VERSION,
        handle: TerminalTaskHandle {
            task_id: TerminalTaskId::new("terminal-test").expect("stable id"),
            command_sha256: "0".repeat(64),
            cwd_label: ".".to_owned(),
            shell_label: "sh".to_owned(),
            shell_sha256: "1".repeat(64),
            log_ref: "terminal-log:test".to_owned(),
            created_at_ms: 1,
            execution_backend: None,
            execution_backend_capabilities: None,
            enforcement_backend: None,
            enforcement_backend_capabilities: None,
            sandbox_profile: None,
        },
        generation: 3,
        status: TerminalTaskStatus::Running,
        readiness: TerminalReadinessStatus::None,
        output_preview: None,
        output_hash: Some("2".repeat(64)),
        output_truncated: false,
        output_total_bytes: 12,
        output_limit_bytes: None,
        output_termination_reason: None,
        cleanup: None,
        updated_at_ms: 4,
    };
    let value = terminal_task_latest_observation(&summary);
    assert_eq!(value["generation"], 3);
    assert_eq!(value["execution_id"], "terminal-test");
    assert_eq!(value["next_action"], "exec_wait");
    assert_eq!(value["output_total_bytes"], 12);
    assert!(value.get("output_preview").is_none());
}
