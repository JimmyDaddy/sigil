//! Real command tools through the agent's audit, persistence and provider-result path.
use std::pin::Pin;

use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::{Value, json};
use sigil_kernel::{
    Agent, AgentRunInput, AgentRunOptions, CompactionConfig, CompletionRequest, ControlEntry,
    InteractionMode, JsonlSessionStore, MemoryConfig, PermissionConfig, PermissionMode, Provider,
    ProviderCapabilities, ProviderChunk, ReasoningStreamSupport, Session, SessionLogEntry,
    TerminalTaskStatus, ToolCall, ToolRegistry,
};

struct ReceiptSequence;

#[async_trait]
impl Provider for ReceiptSequence {
    fn name(&self) -> &str {
        "receipt-sequence"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Unsupported,
            supports_reasoning_effort: false,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let result_for = |id: &str| {
            request
                .messages
                .iter()
                .find(|message| message.tool_call_id.as_deref() == Some(id))
        };
        let calls = if let Some(read) = result_for("read") {
            let output: Value =
                serde_json::from_str(read.content.as_deref().context("read output")?)?;
            ensure!(
                output["projection"]["preview"]
                    .as_str()
                    .is_some_and(|text| text.contains("receipt-ok")),
                "read output was not delivered: {output}"
            );
            ensure!(
                result_for("next").is_some(),
                "remaining batch call was not completed"
            );
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("received".into())),
                Ok(ProviderChunk::Done),
            ])));
        } else if let Some(start) = result_for("start") {
            let output: Value =
                serde_json::from_str(start.content.as_deref().context("start output")?)?;
            let id = output["facts"]["tool_specific"]["execution_id"]
                .as_str()
                .context("execution id")?;
            vec![
                tool_call(
                    "read",
                    "exec_read",
                    json!({"execution_id": id, "offset": 0, "include_content": true}),
                ),
                tool_call(
                    "next",
                    "exec_command",
                    json!({"command": "printf follow-up", "shell": "sh", "yield_time_ms": 5000}),
                ),
            ]
        } else {
            vec![tool_call(
                "start",
                "exec_command",
                json!({"command": "printf receipt-ok", "shell": "sh", "yield_time_ms": 5000}),
            )]
        };
        Ok(Box::pin(stream::iter(
            calls
                .into_iter()
                .map(|call| Ok(ProviderChunk::ToolCallComplete(call)))
                .chain(std::iter::once(Ok(ProviderChunk::Done))),
        )))
    }
}

fn tool_call(id: &str, name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        args_json: args.to_string(),
    }
}

#[tokio::test]
async fn exec_read_receipt_survives_agent_loop_and_delivers_the_entire_batch() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let mut registry = ToolRegistry::new();
    crate::register_builtin_tools_with_paths(
        &mut registry,
        crate::BuiltinToolPaths::workspace_defaults(workspace.path()),
    );
    let agent = Agent::new(ReceiptSequence, registry);
    let store = JsonlSessionStore::new(workspace.path().join("session.jsonl"))?;
    let mut session = Session::load_from_store("receipt-sequence", "fixture", store.clone())?;
    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("inspect command output"),
            AgentRunOptions {
                workspace_root: workspace.path().to_path_buf(),
                max_turns: Some(4),
                tool_timeout_secs: 10,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Headless,
                permission_config: PermissionConfig {
                    mode: PermissionMode::DangerFullAccess,
                    ..Default::default()
                },
                permission_context: Default::default(),
                permission_mode_override: None,
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut sigil_kernel::event::NoopEventHandler,
        )
        .await?;
    assert_eq!(output.result.final_text, "received");
    drop(session);
    let session = Session::load_from_store("receipt-sequence", "fixture", store)?;
    for id in ["start", "read", "next"] {
        assert_eq!(session.entries().iter().filter(|entry| matches!(entry, SessionLogEntry::ToolResultV3(result) if result.call_id == id)).count(), 1, "exactly one durable result for {id}");
    }
    assert!(session.entries().iter().any(|entry| matches!(entry, SessionLogEntry::Control(ControlEntry::TerminalTask(task)) if matches!(task.status, TerminalTaskStatus::Exited { exit_code: Some(0) }))));
    Ok(())
}
