use std::{fs, path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use serde_json::json;
use sigil_kernel::{
    Agent, AssistantMessageKind, CONFIG_VERSION_V2, ConnectionId, ControlEntry, DurableEventType,
    EventClass, JsonlSessionStore, ModelMessage, ModelRef, ResolvedModelRoute, Session,
    StorageRoot, ToolRegistry,
};
use sigil_runtime::{
    LocalSessionLifecycleLimits, LocalSessionLifecycleOperationKind,
    LocalSessionLifecycleRecoveryStatus, LocalSessionLifecycleService, SessionExportV1,
    SessionRetentionPolicy, resolve_sigil_paths,
};
use tempfile::tempdir;

use super::{
    super::{WorkerCommand, WorkerMessage, worker_loop::fork_local_session},
    common::{
        PlannedProvider, spawn_test_worker, spawn_test_worker_with_existing_authority_composition,
        test_authority_composition, test_root_config,
    },
};

fn write_finalized_session(
    path: &Path,
    prompt: &str,
    root_config: &sigil_kernel::RootConfig,
) -> Result<()> {
    let store = JsonlSessionStore::new(path)?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        resolved_model_route: None,
    })?;
    sigil_runtime::bind_session_composition(&mut session, root_config)?;
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
fn managed_worker_lifecycle_service_uses_authority_namespace() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().join("workspace");
    fs::create_dir(&workspace_root)?;
    let state_root = temp.path().join("state");
    fs::create_dir(&state_root)?;
    let mut root_config = test_root_config(&workspace_root, "deepseek", "deepseek-v4-flash");
    root_config.storage.state_root = StorageRoot::Path(state_root.display().to_string());
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);

    fs::create_dir_all(paths.state_root.join("cache"))?;
    let execution_temp_root = temp.path().join("execution-temp");
    fs::create_dir(&execution_temp_root)?;
    let composition = sigil_runtime::r71_authority_composition::compose_runtime_authority(
        &paths.state_root,
        &execution_temp_root,
        sigil_kernel::resource::CanonicalHash::from_bytes([0x71; 32]),
        sigil_kernel::resource::AuthorityGeneration {
            epoch: 1,
            instance_hash: sigil_kernel::resource::CanonicalHash::from_bytes([0x75; 32]),
        },
        Arc::new(sigil_runtime::r71_shadow_planner::ShadowPlannerV1::new(
            sigil_runtime::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        &[sigil_runtime::managed_storage_writer::StorageWriterChannelV1::SessionLifecycleLog],
    )?;
    let writer = Arc::clone(&composition.storage_writer);
    let session_path = paths.session_log_dir.join("session.jsonl");
    fs::create_dir_all(&paths.session_log_dir)?;
    write_finalized_session(&session_path, "managed lifecycle", &root_config)?;
    let service = super::super::worker_loop::local_session_lifecycle_service_for_worker(
        &root_config,
        &workspace_root,
        Some(&writer),
    )
    .expect("managed lifecycle service should attach");
    service.set_session_pin(&session_path, true, 1)?;
    let expected_lifecycle_path = writer
        .managed_named_leaf_path(
            sigil_runtime::managed_storage_writer::StorageWriterChannelV1::SessionLifecycleLog,
            &paths.workspace_id,
        )?
        .join("session-lifecycle-v1.jsonl");
    assert!(
        expected_lifecycle_path.is_file(),
        "managed lifecycle log should use the authority-resolved namespace: {}",
        expected_lifecycle_path.display()
    );
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
    let mut root_config = test_root_config(&workspace_root, "deepseek", "deepseek-v4-flash");
    root_config.config_version = CONFIG_VERSION_V2;
    root_config.agent.runtime_provider.clear();
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
    write_finalized_session(&current_path, "current", &root_config)?;
    write_finalized_session(&target_path, "target", &root_config)?;
    write_finalized_session(&retention_path, "retention", &root_config)?;
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
        worker.recv_until_with_timeout(Duration::from_secs(30), |message| matches!(message, WorkerMessage::LocalSessionInspected { request_id: 11, .. }))?,
        WorkerMessage::LocalSessionInspected { entry, .. }
            if entry.finalized_turn_count == 1 && entry.title.as_deref() == Some("target")
    ));

    worker.send(WorkerCommand::ExportLocalSession {
        request_id: 12,
        source_path: target_path.clone(),
    })?;
    let export_path = match worker.recv_until_with_timeout(Duration::from_secs(30), |message| {
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
        worker.recv_until_with_timeout(Duration::from_secs(30), |message| matches!(
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
        worker.recv_until_with_timeout(Duration::from_secs(30), |message| matches!(
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
    let delete_preview =
        match worker.recv_until_with_timeout(Duration::from_secs(30), |message| {
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
        worker.recv_until_with_timeout(Duration::from_secs(30), |message| matches!(message, WorkerMessage::LocalSessionDeleted { request_id: 16, .. }))?,
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
    let retention_preview =
        match worker.recv_until_with_timeout(Duration::from_secs(30), |message| {
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
        worker.recv_until_with_timeout(Duration::from_secs(30), |message| matches!(message, WorkerMessage::SessionRetentionApplied { request_id: 18, .. }))?,
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
    let invalid_route_error =
        match worker.recv_until_with_timeout(Duration::from_secs(30), |message| {
            matches!(
                message,
                WorkerMessage::LocalSessionLifecycleFailed { request_id: 19, .. }
            )
        })? {
            WorkerMessage::LocalSessionLifecycleFailed { error, .. } => error,
            _ => unreachable!(),
        };
    assert!(
        invalid_route_error.contains("explicit current route"),
        "unexpected invalid-route failure: {invalid_route_error}"
    );

    worker.send(WorkerCommand::ForkLocalSession {
        request_id: 20,
        source_path: current_path,
        current_model_route,
    })?;
    let fork_path = match worker.recv_until_with_timeout(Duration::from_secs(30), |message| {
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

    let managed_writer = worker.managed_storage_writer();
    let service = LocalSessionLifecycleService::new(
        paths.workspace_id.clone(),
        paths.session_log_dir,
        paths.session_exports_root,
    )
    .with_lifecycle_journal_path(paths.session_lifecycle_journal)
    .with_managed_writer(managed_writer, paths.workspace_id)?;
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
    worker.shutdown()?;
    Ok(())
}

#[test]
fn active_session_fork_uses_the_owned_writer_instead_of_catalog_scanning() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().join("workspace");
    fs::create_dir(&workspace_root)?;
    let session_dir = temp.path().join("sessions");
    fs::create_dir(&session_dir)?;
    let source_path = session_dir.join("current.jsonl");
    let mut root_config = test_root_config(&workspace_root, "deepseek", "deepseek-v4-flash");
    root_config.config_version = CONFIG_VERSION_V2;
    root_config.agent.runtime_provider.clear();
    root_config.agent.connection = Some(ConnectionId::new("current-route")?);
    root_config.agent.model = "current-route-model".to_owned();
    root_config.connections.insert(
        "current-route".to_owned(),
        json!({
            "label": "current-route",
            "provider": "deepseek",
            "protocol": "deepseek",
            "base_url": "https://api.deepseek.com",
            "credential": {
                "source": "environment",
                "name": "SIGIL_API_KEY"
            }
        }),
    );
    write_finalized_session(&source_path, "current", &root_config)?;
    let current_model_route =
        sigil_runtime::provider_connections::resolve_default_model_route(&root_config)?.1;
    let active_store = JsonlSessionStore::new(&source_path)?;
    let active_session =
        Session::load_from_store("deepseek", "deepseek-v4-flash", active_store.clone())?;
    let service =
        LocalSessionLifecycleService::new("workspace", &session_dir, temp.path().join("exports"))
            .with_limits(LocalSessionLifecycleLimits {
                max_total_validation_bytes: 0,
                ..LocalSessionLifecycleLimits::default()
            });

    let error = fork_local_session(
        &service,
        &source_path,
        None,
        &root_config,
        &current_model_route,
    )
    .expect_err("an external source should still require a ready catalog entry");
    assert!(error.to_string().contains("not ready"));

    let output = fork_local_session(
        &service,
        &source_path,
        Some((&source_path, &active_session)),
        &root_config,
        &current_model_route,
    )?;
    assert!(output.output.destination_path.is_file());
    assert_eq!(output.output.copied_message_count, 2);
    Ok(())
}

#[test]
fn conversation_fork_selected_turn_uses_shared_lifecycle_without_checkpoint_or_model_call()
-> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let path = sessions.join("source.jsonl");
    let mut config = test_root_config(&workspace, "deepseek", "deepseek-v4-flash");
    let (authority_composition, authority_root) = test_authority_composition(&workspace)?;
    config.config_version = CONFIG_VERSION_V2;
    config.agent.runtime_provider.clear();
    config.agent.connection = Some(ConnectionId::new("selected-route")?);
    config.connections.insert("selected-route".to_owned(), json!({
        "label":"Selected", "provider":"deepseek", "protocol":"deepseek",
        "base_url":"https://api.deepseek.com", "credential":{"source":"environment","name":"SIGIL_API_KEY"}
    }));
    config.session.log_dir = Some(sessions.display().to_string());
    config.storage.state_root =
        StorageRoot::Path(authority_root.path().join("state").display().to_string());
    config.storage.cache_root = StorageRoot::Path(temp.path().join("cache").display().to_string());
    write_finalized_session(&path, "first completed turn", &config)?;
    {
        let store = JsonlSessionStore::new(&path)?;
        let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
        session.append_user_message(ModelMessage::user("second completed turn"))?;
        let answer = ModelMessage::assistant_with_kind(
            Some("second answer".to_owned()),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        );
        session.append_assistant_message(answer.clone())?;
        session.append_durable_event(DurableEventType::RunFinalized, EventClass::Critical, json!({
            "run_status":"completed", "terminal_reason":"final_answer", "final_message_id":answer.id,
            "tool_calls":0, "error":null
        }))?;
    }
    let source_id = Session::load_from_store(
        "deepseek",
        "deepseek-v4-flash",
        JsonlSessionStore::new(&path)?,
    )?
    .session_scope_id()
    .to_owned();
    let target_model_ref =
        sigil_runtime::provider_connections::resolve_default_model_route(&config)?
            .1
            .model_ref;
    let (provider, calls) = PlannedProvider::new_with_stream_start_signal(Vec::new());
    let worker = spawn_test_worker_with_existing_authority_composition(
        config.clone(),
        path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace.clone(),
        Arc::clone(&authority_composition),
    )?;
    worker.send(WorkerCommand::LoadConversationForkPoints {
        request_id: 401,
        source_session_id: source_id.clone(),
    })?;
    let points = match worker
        .recv_until_with_timeout(Duration::from_secs(30), |message| {
            matches!(
                message,
                WorkerMessage::ConversationForkPointsLoaded {
                    request_id: 401,
                    ..
                } | WorkerMessage::LocalSessionLifecycleFailed {
                    request_id: 401,
                    ..
                }
            )
        })
        .context("loading source conversation fork points")?
    {
        WorkerMessage::ConversationForkPointsLoaded { points, .. } => points,
        WorkerMessage::LocalSessionLifecycleFailed { error, .. } => {
            anyhow::bail!("loading source conversation fork points failed: {error}")
        }
        other => panic!("unexpected source response: {other:?}"),
    };
    assert_eq!(points.len(), 2);
    assert_eq!(
        points[0].prompt_preview.as_deref(),
        Some("first completed turn")
    );
    // Unrelated workspace changes cannot act as a conversation-fork permission gate.
    fs::write(
        workspace.join("unrelated.txt"),
        "different workspace observation",
    )?;
    let source_bytes = fs::read(&path)?;
    for (request_id, expected_source, digest) in [
        (
            402,
            "wrong-session".to_owned(),
            points[0].source_turn_digest.clone(),
        ),
        (403, source_id.clone(), "sha256:stale-turn".to_owned()),
    ] {
        worker.send(WorkerCommand::ForkConversation {
            request_id,
            source_session_id: expected_source,
            source_turn_digest: digest,
            target_model_ref: target_model_ref.clone(),
        })?;
        assert!(matches!(worker.recv_until_with_timeout(Duration::from_secs(30), |message|
            matches!(message, WorkerMessage::LocalSessionLifecycleFailed { request_id: id, .. } if *id == request_id))?,
            WorkerMessage::LocalSessionLifecycleFailed { .. }));
    }
    dispatch_bound_fork(
        &worker,
        &source_id,
        &points[0].source_turn_digest,
        &target_model_ref,
        "a",
    )?;
    let branch = match worker
        .recv_until_with_timeout(Duration::from_secs(30), |message| {
            matches!(
                message,
                WorkerMessage::LocalSessionForked {
                    request_id: 404,
                    ..
                } | WorkerMessage::LocalSessionLifecycleFailed {
                    request_id: 404,
                    ..
                }
            )
        })
        .context("forking first selected conversation turn")?
    {
        WorkerMessage::LocalSessionForked {
            session_log_path,
            copied_message_count,
            ..
        } => {
            assert_eq!(copied_message_count, 2);
            session_log_path
        }
        WorkerMessage::LocalSessionLifecycleFailed { error, .. } => {
            anyhow::bail!("forking first selected conversation turn failed: {error}")
        }
        other => panic!("unexpected branch response: {other:?}"),
    };
    assert!(
        fs::read(&path)?.starts_with(&source_bytes),
        "source history must remain an unchanged prefix"
    );
    assert!(
        JsonlSessionStore::read_entries(&path)?
            .iter()
            .any(|entry| matches!(
                entry,
                sigil_kernel::SessionLogEntry::Control(
                    sigil_kernel::ControlEntry::ConversationForkCommittedV1(_)
                )
            )),
        "the completed copy must have a source-side audit"
    );
    let entries = JsonlSessionStore::read_entries(&branch)?;
    assert!(entries.iter().any(
        |entry| matches!(entry, sigil_kernel::SessionLogEntry::User(message)
        if message.content.as_deref() == Some("first completed turn"))
    ));
    assert!(!entries.iter().any(
        |entry| matches!(entry, sigil_kernel::SessionLogEntry::User(message)
        if message.content.as_deref() == Some("second completed turn"))
    ));
    assert!(
        calls.try_recv().is_err(),
        "branching must not dispatch a model call"
    );
    drop(worker);
    // A new process/worker can reuse the UI counter; the distinct durable intent must not
    // reopen the previous branch, even when the source turn is identical.
    let (provider, second_calls) = PlannedProvider::new_with_stream_start_signal(Vec::new());
    let restarted = spawn_test_worker_with_existing_authority_composition(
        config,
        path,
        Agent::new(provider, ToolRegistry::new()),
        workspace,
        authority_composition,
    )?;
    dispatch_bound_fork(
        &restarted,
        &source_id,
        &points[0].source_turn_digest,
        &target_model_ref,
        "b",
    )?;
    let next = restarted
        .recv_until_with_timeout(Duration::from_secs(30), |message| {
            matches!(
                message,
                WorkerMessage::LocalSessionForked {
                    request_id: 404,
                    ..
                } | WorkerMessage::LocalSessionLifecycleFailed {
                    request_id: 404,
                    ..
                }
            )
        })
        .context("forking selected turn after worker restart")?;
    if let WorkerMessage::LocalSessionLifecycleFailed { error, .. } = &next {
        anyhow::bail!("forking selected turn after worker restart failed: {error}");
    }
    let WorkerMessage::LocalSessionForked {
        session_log_path, ..
    } = next
    else {
        unreachable!()
    };
    assert_ne!(
        session_log_path, branch,
        "new K/F must produce a new branch despite the same UI request id"
    );
    assert!(second_calls.try_recv().is_err());
    Ok(())
}

fn dispatch_bound_fork(
    worker: &super::common::TestWorker,
    scope: &str,
    digest: &str,
    model: &ModelRef,
    key_digit: &str,
) -> Result<()> {
    let binding = sigil_kernel::ApplicationOperationBindingV1::new(
        scope.to_owned(),
        key_digit.repeat(64),
        "f".repeat(64),
        sigil_kernel::ApplicationOperationTargetV1::ForkConversation {
            source_turn_digest: digest.to_owned(),
            connection_id: model.connection_id.as_str().to_owned(),
            model_id: model.model_id.clone(),
        },
    )?;
    let (reply, receipt) = std::sync::mpsc::channel();
    worker.send(WorkerCommand::PrepareApplicationOperation {
        binding: Box::new(binding.clone()),
        reply,
    })?;
    receipt
        .recv_timeout(Duration::from_secs(30))?
        .map_err(anyhow::Error::msg)?;
    let (reply, receipt) = std::sync::mpsc::channel();
    worker.send(WorkerCommand::ApplicationDispatch {
        binding: Some(Box::new(binding)),
        run_admission: None,
        command: Box::new(WorkerCommand::ForkConversation {
            request_id: 404,
            source_session_id: scope.to_owned(),
            source_turn_digest: digest.to_owned(),
            target_model_ref: model.clone(),
        }),
        reply,
    })?;
    receipt
        .recv_timeout(Duration::from_secs(30))?
        .map_err(anyhow::Error::msg)?;
    Ok(())
}
