use std::sync::mpsc;

use sigil_kernel::{
    ControlEntry, JsonlSessionStore, SessionLogEntry, TerminalLifecycleEvent,
    TerminalLifecycleUpdateV2, TerminalReadinessStatus, TerminalTaskEntry, TerminalTaskHandle,
    TerminalTaskId, TerminalTaskStatus, terminal_task::TERMINAL_TASK_SCHEMA_VERSION,
};

use super::*;
use crate::runner::worker_event::{WorkerEvent, WorkerReadiness};

#[test]
fn lifecycle_route_persists_before_waking_the_worker() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let log_path = temp.path().join("session.jsonl");
    let (event_tx, event_rx) = mpsc::channel();
    let router = ChannelTerminalLifecycleRouter::new(event_tx);
    let sink = router.sink_for_run(
        "session-a",
        "run-a",
        sigil_kernel::MutationEventRecorder::new(JsonlSessionStore::new(&log_path)?),
    )?;
    let update = lifecycle_update("terminal-a", 1, TerminalTaskStatus::Running)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(sink.publish(update.clone()))?;

    let durable_entries = JsonlSessionStore::read_entries(&log_path)?;
    assert!(durable_entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TerminalTask(task))
                if task.handle.task_id == update.event.task_id
                    && task.generation == update.event.generation
        )
    }));
    let mut readiness = WorkerReadiness::new();
    readiness.ingest(event_rx.recv()?);
    let routed = readiness
        .terminal_lifecycle_updates
        .pop_front()
        .expect("durable lifecycle append should wake the TUI worker");
    assert_eq!(routed.session_scope_id, "session-a");
    assert_eq!(routed.run_id, "run-a");
    assert_eq!(routed.update, update);
    Ok(())
}

#[test]
fn concurrent_runs_freeze_independent_terminal_routes() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let log_path = temp.path().join("session.jsonl");
    let (event_tx, event_rx) = mpsc::channel();
    let router = ChannelTerminalLifecycleRouter::new(event_tx);
    let recorder = sigil_kernel::MutationEventRecorder::new(JsonlSessionStore::new(&log_path)?);
    let first_sink = router.sink_for_run("session-a", "run-a", recorder.clone())?;
    let second_sink = router.sink_for_run("session-a", "run-b", recorder)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        tokio::try_join!(
            first_sink.publish(lifecycle_update(
                "terminal-a",
                1,
                TerminalTaskStatus::Running,
            )?),
            second_sink.publish(lifecycle_update(
                "terminal-b",
                1,
                TerminalTaskStatus::Running,
            )?),
        )?;
        Ok::<(), anyhow::Error>(())
    })?;

    let entries = JsonlSessionStore::read_entries(&log_path)?;
    assert_eq!(terminal_generations(&entries, "terminal-a"), vec![1]);
    assert_eq!(terminal_generations(&entries, "terminal-b"), vec![1]);

    let event = event_rx.recv()?;
    assert!(matches!(&event, WorkerEvent::TerminalLifecycleReady(_)));
    let mut readiness = WorkerReadiness::new();
    readiness.ingest(event);
    let routed = readiness
        .terminal_lifecycle_updates
        .into_iter()
        .map(|update| {
            (
                update.session_scope_id,
                update.run_id,
                update.update.event.task_id,
                update.update.event.generation,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(routed.len(), 2);
    assert!(routed.iter().any(|(session, run, task, generation)| {
        session == "session-a"
            && run == "run-a"
            && task.as_str() == "terminal-a"
            && *generation == 1
    }));
    assert!(routed.iter().any(|(session, run, task, generation)| {
        session == "session-a"
            && run == "run-b"
            && task.as_str() == "terminal-b"
            && *generation == 1
    }));
    Ok(())
}

fn terminal_generations(entries: &[SessionLogEntry], task_id: &str) -> Vec<u64> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TerminalTask(task))
                if task.handle.task_id.as_str() == task_id =>
            {
                Some(task.generation)
            }
            _ => None,
        })
        .collect()
}

fn lifecycle_update(
    task_id: &str,
    generation: u64,
    status: TerminalTaskStatus,
) -> anyhow::Result<TerminalLifecycleUpdateV2> {
    let task_id = TerminalTaskId::new(task_id)?;
    let readiness = TerminalReadinessStatus::None;
    Ok(TerminalLifecycleUpdateV2 {
        event: TerminalLifecycleEvent {
            task_id: task_id.clone(),
            execution_backend: None,
            sandbox_profile: None,
            generation,
            status: status.clone(),
            readiness: readiness.clone(),
            total_output_bytes: generation,
            emitted_at_ms: generation,
        },
        task: TerminalTaskEntry {
            schema_version: TERMINAL_TASK_SCHEMA_VERSION,
            handle: TerminalTaskHandle {
                task_id: task_id.clone(),
                command_sha256: "0".repeat(64),
                cwd_label: ".".to_owned(),
                shell_label: "sh".to_owned(),
                shell_sha256: "1".repeat(64),
                log_ref: format!("terminal-log:{}", task_id.as_str()),
                created_at_ms: 1,
                execution_backend: None,
                execution_backend_capabilities: None,
                enforcement_backend: None,
                enforcement_backend_capabilities: None,
                sandbox_profile: None,
            },
            generation,
            status,
            readiness,
            output_preview: None,
            output_hash: None,
            output_truncated: false,
            output_total_bytes: generation,
            output_limit_bytes: None,
            output_termination_reason: None,
            cleanup: None,
            updated_at_ms: generation,
        },
    })
}
