use std::{fs, thread, time::Duration};

use anyhow::{Result, anyhow};
use sigil_kernel::{
    Agent, ControlEntry, DurableEventType, JsonlSessionStore, ModelMessage, ReasoningEffort,
    SessionLogEntry, SessionStreamRecord, ToolRegistry,
};
use tempfile::tempdir;

use super::{
    super::{WorkerCommand, WorkerMessage},
    common::{
        PlannedProvider, StreamPlan, spawn_test_worker, test_root_config, wait_for_session_entry,
    },
};

#[test]
fn switch_session_restores_identity_and_entries() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let current_log_path = temp.path().join(".sigil/sessions/session-current.jsonl");
    let restore_log_path = temp.path().join(".sigil/sessions/session-restored.jsonl");
    let root_config = test_root_config(&workspace_root, "default-provider", "default-model");
    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, ToolRegistry::new());

    let restore_store = JsonlSessionStore::new(&restore_log_path)?;
    restore_store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "restored-provider".to_owned(),
        model_name: "restored-model".to_owned(),
    }))?;
    restore_store.append(&SessionLogEntry::User(ModelMessage::user(
        "restored prompt",
    )))?;

    let worker = spawn_test_worker(root_config, current_log_path, agent, workspace_root)?;
    worker.send(WorkerCommand::SwitchSession {
        session_log_path: restore_log_path.clone(),
    })?;
    let switched =
        worker.recv_until(|message| matches!(message, WorkerMessage::SessionSwitched { .. }))?;
    assert!(matches!(
        switched,
        WorkerMessage::SessionSwitched {
            ref session_log_path,
            ref provider_name,
            ref model_name,
            ref entries,
        }
            if session_log_path == &restore_log_path
                && provider_name == "restored-provider"
                && model_name == "restored-model"
                && entries.iter().any(|entry| matches!(entry, SessionLogEntry::User(message) if message.content.as_deref() == Some("restored prompt")))
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn start_new_session_creates_empty_session_with_current_identity() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let current_log_path = temp.path().join(".sigil/sessions/session-current.jsonl");
    let new_log_path = temp.path().join(".sigil/sessions/session-new.jsonl");
    let root_config = test_root_config(&workspace_root, "default-provider", "default-model");
    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, ToolRegistry::new());

    let worker = spawn_test_worker(root_config, current_log_path, agent, workspace_root)?;
    worker.send(WorkerCommand::StartNewSession {
        session_log_path: new_log_path.clone(),
    })?;
    let started =
        worker.recv_until(|message| matches!(message, WorkerMessage::NewSessionStarted { .. }))?;

    assert!(matches!(
        started,
        WorkerMessage::NewSessionStarted {
            ref session_log_path,
            ref provider_name,
            ref model_name,
            ref entries,
        }
            if session_log_path == &new_log_path
                && provider_name == "default-provider"
                && model_name == "default-model"
                && entries.len() == 1
                && matches!(entries[0], SessionLogEntry::Control(ControlEntry::SessionIdentity { .. }))
    ));
    let entries = JsonlSessionStore::read_entries(&new_log_path)?;
    assert_eq!(entries.len(), 1);

    worker.shutdown()?;
    Ok(())
}

#[test]
fn invalid_initial_session_log_is_tail_recovered() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-invalid-startup.jsonl");
    fs::create_dir_all(
        session_log_path
            .parent()
            .expect("session log path should have parent"),
    )?;
    fs::write(&session_log_path, "{not-json}\n")?;

    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    wait_for_tail_recovered_event(&session_log_path)?;
    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
        )
    }));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn cancel_run_without_active_task_reports_error() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-idle.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::CancelRun)?;
    let error = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;
    assert!(matches!(
        error,
        WorkerMessage::RunFailed(ref text) if text == "no active run to cancel"
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn cancel_active_run_restores_current_session_from_log() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-cancel.jsonl");
    let expected_session_log_path = session_log_path.clone();
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hang forever".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    wait_for_session_entry(&session_log_path, |entry| {
        matches!(
            entry,
            SessionLogEntry::User(message)
                if message.content.as_deref() == Some("hang forever")
        )
    })?;
    worker.send(WorkerCommand::CancelRun)?;
    let cancelled =
        worker.recv_until(|message| matches!(message, WorkerMessage::RunCancelled { .. }))?;
    assert!(matches!(
        cancelled,
        WorkerMessage::RunCancelled {
            ref session_log_path,
            ref provider_name,
            ref model_name,
            ref entries,
        }
            if session_log_path == &expected_session_log_path
                && provider_name == "planned"
                && model_name == "planned-model"
                && entries.iter().any(|entry| matches!(entry, SessionLogEntry::User(message) if message.content.as_deref() == Some("hang forever")))
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn switch_session_while_active_run_reports_error() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-active.jsonl");
    let restore_log_path = temp.path().join(".sigil/sessions/session-other.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "keep running".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    worker.send(WorkerCommand::SwitchSession {
        session_log_path: restore_log_path,
    })?;
    let error = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;
    assert!(matches!(
        error,
        WorkerMessage::RunFailed(ref text)
            if text == "cannot switch sessions while the agent is running"
    ));

    worker.send(WorkerCommand::CancelRun)?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunCancelled { .. }))?;
    worker.shutdown()?;
    Ok(())
}

#[test]
fn start_new_session_while_active_run_reports_error() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-active.jsonl");
    let new_log_path = temp.path().join(".sigil/sessions/session-new.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "keep running".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    worker.send(WorkerCommand::StartNewSession {
        session_log_path: new_log_path,
    })?;
    let error = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;
    assert!(matches!(
        error,
        WorkerMessage::RunFailed(ref text)
            if text == "cannot start a new session while the agent is running"
    ));

    worker.send(WorkerCommand::CancelRun)?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunCancelled { .. }))?;
    worker.shutdown()?;
    Ok(())
}

#[test]
fn switch_session_reports_load_error_for_missing_session_file() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-current.jsonl");
    let invalid_log_path = temp.path().join(".sigil/sessions");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SwitchSession {
        session_log_path: invalid_log_path.clone(),
    })?;
    let failure = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error)
            if error.contains(&invalid_log_path.display().to_string())
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn switch_session_with_tail_corruption_recovers_and_switches() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-current.jsonl");
    let invalid_log_path = temp.path().join(".sigil/sessions/session-invalid.jsonl");
    fs::create_dir_all(
        invalid_log_path
            .parent()
            .expect("invalid log path should have parent"),
    )?;
    fs::write(&invalid_log_path, "{not-json}\n")?;
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SwitchSession {
        session_log_path: invalid_log_path.clone(),
    })?;
    let switched =
        worker.recv_until(|message| matches!(message, WorkerMessage::SessionSwitched { .. }))?;

    assert!(matches!(
        switched,
        WorkerMessage::SessionSwitched {
            ref session_log_path,
            ref provider_name,
            ref model_name,
            ref entries,
        } if session_log_path == &invalid_log_path
            && provider_name == "planned"
            && model_name == "planned-model"
            && entries.iter().any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
            ))
    ));
    assert!(has_tail_recovered_event(&invalid_log_path)?);

    worker.shutdown()?;
    Ok(())
}

#[test]
fn worker_startup_reports_initial_session_load_failures() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let invalid_session_log_path = temp.path().to_path_buf();
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(
        root_config,
        invalid_session_log_path.clone(),
        agent,
        workspace_root,
    )?;

    let failure = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error)
            if error.contains(&invalid_session_log_path.display().to_string())
    ));
    Ok(())
}

fn has_tail_recovered_event(path: &std::path::Path) -> Result<bool> {
    Ok(JsonlSessionStore::read_event_records(path)?
        .iter()
        .any(|record| {
            matches!(
                record,
                SessionStreamRecord::Stored(event)
                    if event.event_kind() == Some(DurableEventType::LogTailRecovered)
            )
        }))
}

fn wait_for_tail_recovered_event(path: &std::path::Path) -> Result<()> {
    for _ in 0..60 {
        if has_tail_recovered_event(path).unwrap_or(false) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(anyhow!(
        "timed out waiting for tail recovery event in {}",
        path.display()
    ))
}
