use std::{fs, path::Path};

use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    Agent, AssistantMessageKind, ControlEntry, DurableEventType, EventClass, JsonlSessionStore,
    ModelMessage, Session, StorageRoot, ToolRegistry,
};
use sigil_runtime::{SessionRetentionPolicy, resolve_sigil_paths};
use tempfile::tempdir;

use super::{
    super::{WorkerCommand, WorkerMessage},
    common::{PlannedProvider, spawn_test_worker, test_root_config},
};

fn write_finalized_session(path: &Path, prompt: &str) -> Result<()> {
    let store = JsonlSessionStore::new(path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
    })?;
    session.append_user_message(ModelMessage::user(prompt))?;
    let assistant = ModelMessage::assistant_with_kind(
        Some(format!("completed {prompt}")),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    );
    session.append_assistant_message(assistant.clone())?;
    session.append_durable_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({
            "run_status": "completed",
            "terminal_reason": "final_answer",
            "final_message_id": assistant.id,
            "tool_calls": 0,
            "error": null
        }),
    )?;
    Ok(())
}

#[test]
fn worker_routes_request_bound_local_session_lifecycle_operations() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().join("workspace");
    fs::create_dir(&workspace_root)?;
    let session_dir = temp.path().join("sessions");
    fs::create_dir(&session_dir)?;
    let current_path = session_dir.join("current.jsonl");
    let target_path = session_dir.join("target.jsonl");
    write_finalized_session(&current_path, "current")?;
    write_finalized_session(&target_path, "target")?;

    let mut root_config = test_root_config(&workspace_root, "deepseek", "deepseek-v4-flash");
    root_config.session.log_dir = Some(session_dir.display().to_string());
    root_config.storage.state_root =
        StorageRoot::Path(temp.path().join("state").display().to_string());
    root_config.storage.cache_root =
        StorageRoot::Path(temp.path().join("cache").display().to_string());
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    let agent = Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new());
    let worker = spawn_test_worker(root_config, current_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::InspectLocalSession {
        request_id: 11,
        source_path: target_path.clone(),
    })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(message, WorkerMessage::LocalSessionInspected { request_id: 11, .. }))?,
        WorkerMessage::LocalSessionInspected { entry, .. }
            if entry.finalized_turn_count == 1 && entry.title.as_deref() == Some("target")
    ));

    worker.send(WorkerCommand::ExportLocalSession {
        request_id: 12,
        source_path: target_path.clone(),
    })?;
    let export_path = match worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::LocalSessionExported { request_id: 12, .. }
        )
    })? {
        WorkerMessage::LocalSessionExported { output, .. } => output.path,
        _ => unreachable!(),
    };
    assert!(export_path.starts_with(&paths.session_exports_root));
    assert!(export_path.is_file());

    worker.send(WorkerCommand::SetLocalSessionPin {
        request_id: 13,
        source_path: target_path.clone(),
        pinned: true,
    })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(message, WorkerMessage::LocalSessionPinChanged { request_id: 13, .. }))?,
        WorkerMessage::LocalSessionPinChanged { entry, .. } if entry.pinned
    ));
    worker.send(WorkerCommand::SetLocalSessionPin {
        request_id: 14,
        source_path: target_path.clone(),
        pinned: false,
    })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(message, WorkerMessage::LocalSessionPinChanged { request_id: 14, .. }))?,
        WorkerMessage::LocalSessionPinChanged { entry, .. } if !entry.pinned
    ));

    worker.send(WorkerCommand::PreviewLocalSessionDelete {
        request_id: 15,
        source_path: target_path.clone(),
    })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(message, WorkerMessage::LocalSessionDeletePreviewed { request_id: 15, .. }))?,
        WorkerMessage::LocalSessionDeletePreviewed { preview, .. }
            if preview.source_session_ref.as_path() == Path::new("target.jsonl")
    ));

    worker.send(WorkerCommand::PreviewSessionRetention {
        request_id: 16,
        policy: SessionRetentionPolicy {
            max_sessions: Some(1),
            max_bytes: None,
            expire_older_than_ms: None,
        },
    })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(message, WorkerMessage::SessionRetentionPreviewed { request_id: 16, .. }))?,
        WorkerMessage::SessionRetentionPreviewed { preview, .. }
            if preview.candidates.len() == 1
                && preview.candidates[0].delete_preview.source_session_ref.as_path()
                    == Path::new("target.jsonl")
    ));

    worker.send(WorkerCommand::ForkLocalSession {
        request_id: 17,
        source_path: target_path,
    })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(message, WorkerMessage::LocalSessionForked { request_id: 17, .. }))?,
        WorkerMessage::LocalSessionForked {
            session_log_path,
            copied_message_count: 2,
            ..
        } if session_log_path.is_file()
    ));

    worker.shutdown()?;
    Ok(())
}
