use std::{
    sync::{Arc, mpsc},
    time::Duration,
};

use anyhow::{Context, Result};
use sigil_kernel::{
    ControlEntry, JsonlSessionStore, Session, SessionLogEntry, SessionRef, ToolArtifactGcRootsV1,
    ToolArtifactSensitivity, ToolArtifactStore,
};
use sigil_runtime::LocalSessionLifecycleService;

use super::super::{
    ManagedTuiArtifactStoreLease,
    worker_event::{WorkerEvent, WorkerEventPayloadSender},
    worker_loop::ArtifactGcTaskManager,
};
use super::common::test_authority_composition;

#[test]
fn gc_task_runs_behind_one_typed_completion_event() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    let session_path = sessions.join("session.jsonl");
    let session_store = JsonlSessionStore::new(&session_path)?;
    session_store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        resolved_model_route: None,
    }))?;
    session_store.append(&SessionLogEntry::Control(ControlEntry::Note {
        kind: "artifact_gc_fixture".to_owned(),
        data: serde_json::Value::Null,
    }))?;
    let session = Session::load_from_store("deepseek", "deepseek-v4-flash", session_store)?;
    let session_path = session.store_path().context("fixture must be durable")?;
    let session_scope_id = session.session_scope_id().to_owned();
    let (composition, _authority_root) = test_authority_composition(temp.path())?;
    let artifact_lease = ManagedTuiArtifactStoreLease::acquire(
        Arc::clone(&composition.storage_writer),
        session_path,
        &session_scope_id,
    )?;
    let artifact_store: ToolArtifactStore = artifact_lease.store();
    let descriptor = artifact_store.capture_text(
        "call-1",
        "shell",
        "durable output",
        ToolArtifactSensitivity::Ordinary,
    )?;
    let roots = ToolArtifactGcRootsV1 {
        active_result_refs: [descriptor.artifact_ref].into_iter().collect(),
        ..ToolArtifactGcRootsV1::default()
    };
    assert_eq!(artifact_store.session_scope_id(), session_scope_id);
    let held_lease = artifact_lease.maintenance_lease();
    drop(artifact_lease);
    let lifecycle =
        LocalSessionLifecycleService::new("workspace", &sessions, temp.path().join("exports"))
            .with_lifecycle_journal_path(temp.path().join("lifecycle.jsonl"))
            .with_managed_writer(Arc::clone(&composition.storage_writer), "workspace")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (event_tx, event_rx) = mpsc::channel();
    let mut tasks = ArtifactGcTaskManager::new();
    let attachment = Arc::new(
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            session_path,
        )?,
    );

    for request_id in [7, 8] {
        tasks.start(
            &runtime,
            request_id,
            session_scope_id.clone(),
            Arc::clone(&attachment),
            None,
            WorkerEventPayloadSender::artifact_gc(event_tx.clone()),
            lifecycle.clone(),
            SessionRef::new_relative("session.jsonl")?,
            roots.clone(),
            Some(Arc::clone(&held_lease)),
        );

        let event = event_rx
            .recv_timeout(Duration::from_secs(30))
            .context("artifact GC did not publish its completion event within 30 seconds")?;
        let WorkerEvent::ArtifactGcCompleted(result) = event else {
            panic!("artifact GC must publish one typed completion event");
        };
        assert_eq!(result.request_id, request_id);
        assert!(tasks.accept_result(result.request_id, &result.session_scope_id));
        let report = result.result.map_err(anyhow::Error::msg)?;
        assert_eq!(report.scanned_manifests, 1);
        assert_eq!(report.retained_manifests, 1);
        assert_eq!(report.tombstoned_manifests, 0);
        assert!(event_rx.try_recv().is_err());
    }
    Ok(())
}

#[test]
fn held_artifact_gc_after_cancelled_run_and_promoted_real_read_uses_existing_lease() -> Result<()> {
    use super::super::{WorkerCommand, WorkerMessage};
    use super::common::{
        PlannedProvider, StreamPlan, failing_role_provider_builder,
        routed_unauthenticated_test_root_config, spawn_test_worker_with_owned_terminal,
    };
    use sigil_kernel::{
        Agent, ConversationInputKind, ConversationInputTarget, ProviderChunk, ReasoningEffort,
        RuntimeCompositionConfig, ToolCall, ToolRegistry,
    };

    let registration_runtime = tokio::runtime::Runtime::new()?;
    let _runtime_guard = registration_runtime.enter();
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().to_path_buf();
    std::fs::write(
        workspace.join("note.txt"),
        "first\ncomplete promoted output\nlast\n",
    )?;
    let mut config = routed_unauthenticated_test_root_config(&workspace, "planned-model");
    config.composition = RuntimeCompositionConfig::core();
    let (authority, _authority_root) = test_authority_composition(&workspace)?;
    config.storage.state_root =
        sigil_kernel::StorageRoot::Path(_authority_root.path().join("state").display().to_string());
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(workspace.join("cache").display().to_string());
    config.save(&workspace.join("sigil.toml"))?;
    let config = config.with_effective_composition()?;
    let source = sigil_runtime::resolve_sigil_paths(&config.storage, &config.session, &workspace)
        .session_log_dir
        .join("session-promoted.jsonl");
    let paths = sigil_tools_builtin::BuiltinToolPaths::workspace_defaults(&workspace);
    let scratch = sigil_runtime::authority_scratch_control(paths.scratch_root.clone());
    let mut registry = ToolRegistry::new();
    let _handles = sigil_tools_builtin::register_builtin_tools_with_selection(
        &mut registry,
        paths,
        Arc::clone(&authority.command_execution)
            as Arc<dyn sigil_tools_builtin::ManagedCommandExecutionPortV1>,
        sigil_tools_builtin::BuiltinToolSelection::core(),
        Some(scratch),
        || sigil_tools_builtin::BuiltinTerminalOptions {
            execution_config: sigil_tools_builtin::TerminalExecutionConfig::from_execution_config(
                &config.execution,
            ),
            lifecycle_route: None,
            executor: Arc::clone(&authority.command_execution)
                as Arc<dyn sigil_tools_builtin::ManagedTerminalExecutionPortV1>,
        },
    );
    let args_json = serde_json::json!({"path":"note.txt"}).to_string();
    let gate = Arc::new(tokio::sync::Notify::new());
    let (provider, started) = PlannedProvider::new_with_stream_start_signal(vec![
        StreamPlan::GatedChunks {
            gate,
            chunks: vec![ProviderChunk::Done],
        },
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "promoted-read".to_owned(),
                name: "read_file".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "promoted-read".to_owned(),
                delta: args_json.clone(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "promoted-read".to_owned(),
                name: "read_file".to_owned(),
                args_json,
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("read complete".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    std::thread::scope(|threads| -> Result<()> {
        let (up_tx, up_rx) = mpsc::channel::<WorkerEvent>();
        let (down_tx, down_rx) = mpsc::channel();
        let (gc_tx, gc_rx) = mpsc::channel();
        // Observe and forward the real worker event unchanged; no GC completion is synthesized.
        threads.spawn(move || {
            while let Ok(event) = up_rx.recv() {
                if let WorkerEvent::ArtifactGcCompleted(result) = &event {
                    let sequence = result
                        .projection_cursor
                        .as_ref()
                        .map_or(0, |cursor| cursor.last_applied_stream_sequence);
                    let outcome = result.result.as_ref().map(|_| ()).map_err(Clone::clone);
                    let _ = gc_tx.send((sequence, outcome));
                }
                if down_tx.send(event).is_err() {
                    break;
                }
            }
        });
        let worker = spawn_test_worker_with_owned_terminal(
            config,
            source.clone(),
            Agent::new(provider, registry),
            workspace.clone(),
            failing_role_provider_builder(),
            authority,
            None,
            None,
            Some((up_tx, down_rx)),
        )?;
        worker.send(WorkerCommand::SubmitPrompt {
            prompt: "hold this first response".to_owned(),
            reasoning_effort: ReasoningEffort::High,
        })?;
        started
            .recv_timeout(Duration::from_secs(10))
            .context("initial gated run must enter the provider before queuing its follow-up")?;
        worker.send(WorkerCommand::QueueConversationInput {
            prompt: "read note.txt completely".to_owned(),
            kind: ConversationInputKind::Chat,
            target: ConversationInputTarget::MainThread,
            reasoning_effort: ReasoningEffort::High,
        })?;
        worker.recv_until(|message| {
            matches!(message,
                WorkerMessage::ConversationQueueUpdated { items, .. } if items.len() == 1
            )
        })?;
        worker.send(WorkerCommand::CancelRun)?;
        let finished = worker.recv_until_with_timeout(Duration::from_secs(20), |message| {
            matches!(message,
                WorkerMessage::RunFinished { result, .. } if result.final_text == "read complete"
            )
        })?;
        let WorkerMessage::RunFinished { entries, .. } = finished else {
            unreachable!()
        };
        assert!(entries.iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(_))
        )));
        let result = entries
            .iter()
            .find_map(|entry| match entry {
                SessionLogEntry::ToolResultV3(result) if result.call_id == "promoted-read" => {
                    Some(result)
                }
                _ => None,
            })
            .context("promoted real read must publish a V3 result")?;
        assert_eq!(result.facts.status, "ok");
        assert!(
            result.artifact.descriptor().is_some(),
            "complete output must remain published"
        );
        let records = JsonlSessionStore::read_event_records(&source)?;
        let read_sequence = records
            .iter()
            .find_map(|record| {
                let event = record.stored_event();
                (event.payload["session_log_entry"]["tool_result_v3"]["call_id"] == "promoted-read")
                    .then_some(event.stream_sequence)
            })
            .context("real read source sequence")?;
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut observed_gc_sequences = Vec::new();
        loop {
            let (sequence, result) = gc_rx
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .with_context(|| {
                    format!(
                        "post-read GC did not reach source sequence {read_sequence}; observed GC sequences: {observed_gc_sequences:?}"
                    )
                })?;
            observed_gc_sequences.push(sequence);
            result.map_err(anyhow::Error::msg).context(
                "real worker GC must retain the promoted read's existing artifact lease",
            )?;
            if sequence >= read_sequence {
                break;
            }
        }
        worker.shutdown()?;
        Ok(())
    })
}
