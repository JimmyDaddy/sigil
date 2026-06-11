use anyhow::Result;
use sigil_kernel::{
    Agent, ControlEntry, JsonlSessionStore, ModelMessage, ProviderChunk, ReasoningEffort,
    SessionLogEntry, ToolRegistry, UsageStats,
};
use tempfile::tempdir;

use super::{
    super::{CompactionTrigger, WorkerCommand, WorkerMessage},
    common::{PlannedProvider, StreamPlan, spawn_test_worker, test_root_config},
};

#[test]
fn compact_now_persists_record_and_restores_session_view() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-compact.jsonl");
    let expected_session_log_path = session_log_path.clone();
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.compaction.tail_messages = 2;
    let store = JsonlSessionStore::new(&session_log_path)?;
    store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "planned".to_owned(),
        model_name: "planned-model".to_owned(),
    }))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("one")))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("two".to_owned()),
        Vec::new(),
    )))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("three")))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("four".to_owned()),
        Vec::new(),
    )))?;

    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::CompactNow)?;
    let compacted =
        worker.recv_until(|message| matches!(message, WorkerMessage::SessionCompacted { .. }))?;
    assert!(matches!(
        compacted,
        WorkerMessage::SessionCompacted { ref session_log_path, trigger, ref entries, .. }
            if session_log_path == &expected_session_log_path
                && trigger == CompactionTrigger::Manual
                && entries.iter().any(|entry| matches!(entry, SessionLogEntry::Control(ControlEntry::CompactionApplied(_))))
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn compact_now_is_rejected_while_run_is_active() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-compact-busy.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hold".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;

    worker.send(WorkerCommand::CompactNow)?;
    let error = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    assert!(matches!(
        error,
        WorkerMessage::RunFailed(ref text)
            if text == "cannot compact while the agent is running"
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn compact_now_without_enough_history_reports_error() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-compact-empty.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::CompactNow)?;
    let error = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    assert!(matches!(
        error,
        WorkerMessage::RunFailed(ref text)
            if text.contains("session does not have enough history to compact")
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn hard_threshold_run_is_auto_compacted_after_finish() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-auto-compact.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.compaction.context_window_tokens = Some(100);
    root_config.compaction.soft_threshold_ratio = 0.5;
    root_config.compaction.hard_threshold_ratio = 0.8;
    root_config.compaction.tail_messages = 1;

    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::Usage(UsageStats {
            prompt_tokens: 90,
            completion_tokens: 12,
            cache_hit_tokens: 0,
            cache_miss_tokens: 90,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        }),
        ProviderChunk::TextDelta("finished turn".to_owned()),
        ProviderChunk::Done,
    ])]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hello".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    let compacted =
        worker.recv_until(|message| matches!(message, WorkerMessage::SessionCompacted { .. }))?;
    assert!(matches!(
        compacted,
        WorkerMessage::SessionCompacted { trigger, ref record, ref entries, .. }
            if trigger == CompactionTrigger::AutomaticHardThreshold
                && record.compacted_message_count == 1
                && entries.iter().any(|entry| matches!(entry, SessionLogEntry::Control(ControlEntry::CompactionApplied(saved)) if saved == record))
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn provider_context_window_prevents_early_auto_compaction() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-provider-window.jsonl");
    let mut root_config = test_root_config(&workspace_root, "deepseek", "deepseek-v4-pro");
    root_config.compaction.context_window_tokens = Some(128_000);
    root_config.compaction.soft_threshold_ratio = 0.5;
    root_config.compaction.hard_threshold_ratio = 0.8;
    root_config.compaction.tail_messages = 1;

    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::Usage(UsageStats {
            prompt_tokens: 90_354,
            completion_tokens: 12,
            cache_hit_tokens: 0,
            cache_miss_tokens: 90_354,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        }),
        ProviderChunk::TextDelta("finished turn".to_owned()),
        ProviderChunk::Done,
    ])]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hello".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    assert!(matches!(
        finished,
        WorkerMessage::RunFinished { ref entries, .. }
            if !entries.iter().any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::CompactionApplied(_))
            ))
    ));

    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::CompactionApplied(_))
    )));

    worker.shutdown()?;
    Ok(())
}
