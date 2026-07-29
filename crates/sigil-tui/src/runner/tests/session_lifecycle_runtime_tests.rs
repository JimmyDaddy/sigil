use std::{fs, path::Path};

use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    Agent, AssistantMessageKind, CONFIG_VERSION_V2, ConnectionId, ControlEntry, DurableEventType,
    EventClass, JsonlSessionStore, ModelMessage, ModelRef, ResolvedModelRoute, Session,
    StorageRoot, ToolRegistry,
};
use sigil_runtime::{
    LocalSessionLifecycleOperationKind, LocalSessionLifecycleRecoveryStatus,
    LocalSessionLifecycleService, SessionExportV1, SessionRetentionPolicy, resolve_sigil_paths,
};
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
        resolved_model_route: None,
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
    let retention_path = session_dir.join("retention.jsonl");
    write_finalized_session(&current_path, "current")?;
    write_finalized_session(&target_path, "target")?;
    write_finalized_session(&retention_path, "retention")?;

    let mut root_config = test_root_config(&workspace_root, "deepseek", "deepseek-v4-flash");
    root_config.config_version = Some(CONFIG_VERSION_V2);
    root_config.agent.provider.clear();
    root_config.agent.connection = Some(ConnectionId::new("saved-default")?);
    root_config.agent.model = "saved-default-model".to_owned();
    for connection_id in ["saved-default", "current-route"] {
        root_config.connections.insert(
            connection_id.to_owned(),
            json!({
                "label": connection_id,
                "provider": "deepseek",
                "protocol": "deepseek",
                "base_url": "https://api.deepseek.com",
                "credential": {
                    "source": "environment",
                    "name": "SIGIL_API_KEY"
                }
            }),
        );
    }
    root_config.session.log_dir = Some(session_dir.display().to_string());
    root_config.storage.state_root =
        StorageRoot::Path(temp.path().join("state").display().to_string());
    root_config.storage.cache_root =
        StorageRoot::Path(temp.path().join("cache").display().to_string());
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    let mut current_route_config = root_config.clone();
    current_route_config.agent.connection = Some(ConnectionId::new("current-route")?);
    current_route_config.agent.model = "current-route-model".to_owned();
    let current_model_route =
        sigil_runtime::provider_connections::resolve_default_model_route(&current_route_config)?.1;
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
    let export: SessionExportV1 = serde_json::from_slice(&fs::read(&export_path)?)?;
    export.validate_digest()?;
    assert_eq!(export.payload.messages.len(), 2);

    worker.send(WorkerCommand::SetLocalSessionPin {
        request_id: 13,
        source_path: target_path.clone(),
        pinned: true,
    })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(
            message,
            WorkerMessage::LocalSessionPinChanged { request_id: 13, .. }
                | WorkerMessage::LocalSessionLifecycleFailed { request_id: 13, .. }
        ))?,
        WorkerMessage::LocalSessionPinChanged { entry, .. } if entry.pinned
    ));
    worker.send(WorkerCommand::SetLocalSessionPin {
        request_id: 14,
        source_path: target_path.clone(),
        pinned: false,
    })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(
            message,
            WorkerMessage::LocalSessionPinChanged { request_id: 14, .. }
                | WorkerMessage::LocalSessionLifecycleFailed { request_id: 14, .. }
        ))?,
        WorkerMessage::LocalSessionPinChanged { entry, .. } if !entry.pinned
    ));

    worker.send(WorkerCommand::PreviewLocalSessionDelete {
        request_id: 15,
        source_path: target_path.clone(),
    })?;
    let delete_preview = match worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::LocalSessionDeletePreviewed { request_id: 15, .. }
        )
    })? {
        WorkerMessage::LocalSessionDeletePreviewed { preview, .. } => preview,
        _ => unreachable!(),
    };
    assert_eq!(
        delete_preview.source_session_ref.as_path(),
        Path::new("target.jsonl")
    );
    worker.send(WorkerCommand::ApplyLocalSessionDelete {
        request_id: 16,
        preview: delete_preview,
    })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(message, WorkerMessage::LocalSessionDeleted { request_id: 16, .. }))?,
        WorkerMessage::LocalSessionDeleted { output, .. }
            if output.source_session_ref.as_path() == Path::new("target.jsonl")
    ));
    assert!(!target_path.exists());

    worker.send(WorkerCommand::PreviewSessionRetention {
        request_id: 17,
        policy: SessionRetentionPolicy {
            max_sessions: Some(1),
            max_bytes: None,
            expire_older_than_ms: None,
        },
    })?;
    let retention_preview = match worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::SessionRetentionPreviewed { request_id: 17, .. }
        )
    })? {
        WorkerMessage::SessionRetentionPreviewed { preview, .. } => preview,
        _ => unreachable!(),
    };
    assert_eq!(retention_preview.candidates.len(), 1);
    assert_eq!(
        retention_preview.candidates[0]
            .delete_preview
            .source_session_ref
            .as_path(),
        Path::new("retention.jsonl")
    );
    worker.send(WorkerCommand::ApplySessionRetention {
        request_id: 18,
        preview: retention_preview,
    })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(message, WorkerMessage::SessionRetentionApplied { request_id: 18, .. }))?,
        WorkerMessage::SessionRetentionApplied { output, .. }
            if output.deleted_sessions == 1
    ));
    assert!(!retention_path.exists());

    let invalid_current_route = ResolvedModelRoute::new(
        ModelRef::new(ConnectionId::new("missing-route")?, "missing-model")?,
        "deepseek",
        "deepseek",
        "missing-route-fingerprint",
    )?;
    worker.send(WorkerCommand::ForkLocalSession {
        request_id: 19,
        source_path: current_path.clone(),
        current_model_route: invalid_current_route,
    })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(
            message,
            WorkerMessage::LocalSessionLifecycleFailed { request_id: 19, .. }
        ))?,
        WorkerMessage::LocalSessionLifecycleFailed { error, .. }
            if error.contains("explicit current route")
    ));

    worker.send(WorkerCommand::ForkLocalSession {
        request_id: 20,
        source_path: current_path,
        current_model_route,
    })?;
    let fork_path = match worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::LocalSessionForked { request_id: 20, .. }
        )
    })? {
        WorkerMessage::LocalSessionForked {
            session_log_path,
            copied_message_count: 2,
            ..
        } if session_log_path.is_file() => session_log_path,
        message => panic!("unexpected fork response: {message:?}"),
    };
    let fork_entries = JsonlSessionStore::read_entries(&fork_path)?;
    assert!(fork_entries.iter().any(|entry| {
        matches!(
            entry,
            sigil_kernel::SessionLogEntry::Control(ControlEntry::SessionIdentity {
                resolved_model_route: Some(route),
                ..
            }) if route.model_ref.connection_id.as_str() == "current-route"
                && route.model_ref.model_id == "current-route-model"
        )
    }));

    worker.shutdown()?;
    let service = LocalSessionLifecycleService::new(
        paths.workspace_id,
        paths.session_log_dir,
        paths.session_exports_root,
    )
    .with_lifecycle_journal_path(paths.session_lifecycle_journal);
    let recovery = service.lifecycle_recovery()?;
    assert!(recovery.iter().any(|entry| {
        entry.kind == LocalSessionLifecycleOperationKind::Export
            && entry.status == LocalSessionLifecycleRecoveryStatus::Completed
    }));
    assert!(recovery.iter().any(|entry| {
        entry.kind == LocalSessionLifecycleOperationKind::Delete
            && entry.status == LocalSessionLifecycleRecoveryStatus::Completed
    }));
    assert!(recovery.iter().any(|entry| {
        entry.kind == LocalSessionLifecycleOperationKind::Retention
            && entry.status == LocalSessionLifecycleRecoveryStatus::Completed
    }));
    Ok(())
}
