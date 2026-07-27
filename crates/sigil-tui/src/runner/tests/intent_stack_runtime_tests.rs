use anyhow::Result;
use sigil_kernel::{
    Agent, IntentDigest, IntentDropRequestV1, IntentOperationId, IntentStackVersion,
    PermissionMode, PublicIntentStackStateV1, ToolRegistry,
};
use tempfile::tempdir;

use super::{
    super::{WorkerCommand, WorkerMessage},
    common::{PlannedProvider, spawn_test_worker, test_root_config},
};

fn drop_request() -> IntentDropRequestV1 {
    IntentDropRequestV1 {
        operation_id: IntentOperationId::new("operation_drop_leaf").expect("operation id"),
        stack_version: IntentStackVersion::new(1).expect("stack version"),
        preview_digest: IntentDigest::new(format!("sha256:jcs-v1:{}", "a".repeat(64)))
            .expect("preview digest"),
    }
}

#[test]
fn intent_stack_history_and_permission_boundaries_survive_worker_restart() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-intent-stack.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.permission.mode = PermissionMode::ReadOnly;

    let worker = spawn_test_worker(
        root_config.clone(),
        session_log_path.clone(),
        Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
        workspace_root.clone(),
    )?;
    worker.send(WorkerCommand::LoadIntentStack { request_id: 1 })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(
            message,
            WorkerMessage::IntentStackLoaded { request_id: 1, .. }
        ))?,
        WorkerMessage::IntentStackLoaded {
            request_id: 1,
            stack_state: PublicIntentStackStateV1::HistoryUnavailable { .. },
        }
    ));
    worker.shutdown()?;

    let worker = spawn_test_worker(
        root_config,
        session_log_path,
        Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
        workspace_root,
    )?;
    worker.send(WorkerCommand::LoadIntentStack { request_id: 2 })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(
            message,
            WorkerMessage::IntentStackLoaded { request_id: 2, .. }
        ))?,
        WorkerMessage::IntentStackLoaded {
            request_id: 2,
            stack_state: PublicIntentStackStateV1::HistoryUnavailable { .. },
        }
    ));

    worker.send(WorkerCommand::ExecuteIntentDrop {
        request_id: 3,
        request: drop_request(),
    })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(
            message,
            WorkerMessage::IntentStackOperationFailed { request_id: 3, .. }
        ))?,
        WorkerMessage::IntentStackOperationFailed {
            request_id: 3,
            error,
        } if error.contains("read-only permission mode denies Intent drop")
    ));
    worker.shutdown()
}
