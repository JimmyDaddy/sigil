use anyhow::Result;
use tempfile::tempdir;
use termquill_kernel::{
    Agent, ProviderChunk, ReasoningEffort, RunEvent, SessionLogEntry, ToolRegistry,
};

use super::{
    super::{WorkerCommand, WorkerMessage},
    common::{PlannedProvider, StreamPlan, spawn_test_worker, test_root_config},
};

#[test]
fn submit_prompt_emits_started_event_and_finished_messages() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".termquill/sessions/session-worker.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::TextDelta("hello from worker".to_owned()),
        ProviderChunk::Done,
    ])]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hello".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let started = worker.recv()?;
    assert!(matches!(
        started,
        WorkerMessage::RunStarted { ref prompt } if prompt == "hello"
    ));

    let text_event = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::TextDelta(delta) if delta == "hello from worker")
        )
    })?;
    assert!(matches!(
        text_event,
        WorkerMessage::Event(event)
            if matches!(event.as_ref(), RunEvent::TextDelta(delta) if delta == "hello from worker")
    ));

    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    assert!(matches!(
        finished,
        WorkerMessage::RunFinished { ref result, ref entries }
            if result.final_text == "hello from worker"
                && result.tool_calls == 0
                && entries.iter().any(|entry| matches!(entry, SessionLogEntry::User(message) if message.content.as_deref() == Some("hello")))
    ));

    worker.shutdown()?;
    Ok(())
}
