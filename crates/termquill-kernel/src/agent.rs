use std::collections::BTreeMap;

use anyhow::{Context, Result};
use futures::StreamExt;
use thiserror::Error;

use crate::{
    approval::{ApprovalHandler, AutoApproveHandler, ToolApproval},
    config::{CompactionConfig, MemoryConfig},
    event::{EventHandler, RunEvent},
    permission::{ApprovalMode, InteractionMode, PermissionConfig, PermissionPolicy},
    provider::{ModelMessage, Provider, ProviderChunk, ProviderContinuationState, ToolCall},
    session::{ControlEntry, Session},
    tool::{ToolContext, ToolPreview, ToolRegistry, ToolResultMeta},
};

/// Runtime knobs for one agent run.
#[derive(Debug, Clone)]
pub struct AgentRunOptions {
    pub workspace_root: std::path::PathBuf,
    pub max_turns: usize,
    pub tool_timeout_secs: u64,
    pub reasoning_effort: Option<crate::provider::ReasoningEffort>,
    pub traffic_partition_key: Option<String>,
    pub interaction_mode: InteractionMode,
    pub permission_config: PermissionConfig,
    pub memory_config: MemoryConfig,
    pub compaction_config: CompactionConfig,
}

/// Final aggregate result from one completed agent run.
#[derive(Debug, Clone)]
pub struct AgentRunResult {
    pub final_text: String,
    pub tool_calls: usize,
}

/// Stable agent loop failure modes.
#[derive(Debug, Error)]
pub enum AgentError {
    #[error("agent exceeded max turns without reaching a final answer")]
    MaxTurnsExceeded,
}

/// Provider-backed agent loop with a registered tool surface.
pub struct Agent<P> {
    provider: P,
    tools: ToolRegistry,
}

impl<P> Agent<P>
where
    P: Provider,
{
    /// Creates a new agent from one provider implementation and tool registry.
    pub fn new(provider: P, tools: ToolRegistry) -> Self {
        Self { provider, tools }
    }

    /// Runs the agent with automatic tool approval.
    ///
    /// # Errors
    ///
    /// Returns an error when session persistence fails, request building fails, the provider
    /// stream errors, the event sink fails, or a tool execution path fails before it can be
    /// surfaced as a structured tool result.
    pub async fn run(
        &self,
        session: &mut Session,
        prompt: impl Into<String>,
        options: AgentRunOptions,
        handler: &mut (impl EventHandler + Send),
    ) -> Result<AgentRunResult> {
        let mut approval_handler = AutoApproveHandler;
        self.run_with_approval(session, prompt, options, handler, &mut approval_handler)
            .await
    }

    /// Runs the agent with an explicit approval handler for mutating tools.
    ///
    /// # Errors
    ///
    /// Returns an error when session persistence fails, request building fails, the provider
    /// stream errors, the event sink fails, or the approval handler itself errors.
    pub async fn run_with_approval<H, A>(
        &self,
        session: &mut Session,
        prompt: impl Into<String>,
        options: AgentRunOptions,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<AgentRunResult>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        session.append_user_message(ModelMessage::user(prompt.into()))?;

        let mut previous_response_handle = session.latest_response_handle(self.provider.name());
        let mut total_tool_calls = 0usize;

        for _ in 0..options.max_turns {
            let request = session.build_request(
                &options.workspace_root,
                &options.memory_config,
                self.tools.specs(),
                options.reasoning_effort.clone(),
                previous_response_handle.clone(),
                options.traffic_partition_key.clone(),
            )?;

            let mut stream = self.provider.stream(request).await?;
            let mut assistant_text = String::new();
            let mut reasoning_buffer = String::new();
            let mut tool_parts: BTreeMap<String, (String, String)> = BTreeMap::new();
            let mut completed_calls: Vec<ToolCall> = Vec::new();
            let mut pending_states: Vec<ProviderContinuationState> = Vec::new();

            while let Some(chunk) = stream.next().await {
                match chunk.context("provider stream failed")? {
                    ProviderChunk::TextDelta(delta) => {
                        assistant_text.push_str(&delta);
                        handler.handle(RunEvent::TextDelta(delta))?;
                    }
                    ProviderChunk::ReasoningDelta(delta) => {
                        reasoning_buffer.push_str(&delta);
                        handler.handle(RunEvent::ReasoningDelta(delta))?;
                    }
                    ProviderChunk::ReasoningSummaryDelta(delta) => {
                        handler.handle(RunEvent::ReasoningDelta(delta))?;
                    }
                    ProviderChunk::ToolCallStart { id, name } => {
                        tool_parts.insert(id.clone(), (name.clone(), String::new()));
                        handler.handle(RunEvent::ToolCallStarted(ToolCall {
                            id,
                            name,
                            args_json: String::new(),
                        }))?;
                    }
                    ProviderChunk::ToolCallArgsDelta { id, delta } => {
                        if let Some((_, args_json)) = tool_parts.get_mut(&id) {
                            args_json.push_str(&delta);
                        }
                        handler.handle(RunEvent::ToolCallArgsDelta { id, delta })?;
                    }
                    ProviderChunk::ToolCallComplete(call) => {
                        completed_calls.push(call.clone());
                        handler.handle(RunEvent::ToolCallCompleted(call))?;
                    }
                    ProviderChunk::Usage(usage) => {
                        session.stats_mut().apply_usage(&usage);
                        session.append_control(ControlEntry::UsageSnapshot(usage.clone()))?;
                        handler.handle(RunEvent::Usage(usage))?;
                    }
                    ProviderChunk::ResponseHandle(handle) => {
                        previous_response_handle = Some(handle.clone());
                        let control = ControlEntry::ResponseHandleTracked(handle);
                        session.append_control(control.clone())?;
                        handler.handle(RunEvent::Control(control))?;
                    }
                    ProviderChunk::BackgroundTaskAccepted(handle) => {
                        let control = ControlEntry::BackgroundTaskTracked(handle);
                        session.append_control(control.clone())?;
                        handler.handle(RunEvent::Control(control))?;
                    }
                    ProviderChunk::BackgroundTaskStatus(status) => {
                        handler.handle(RunEvent::Notice(format!(
                            "background task {} status {}",
                            status.task_id, status.status
                        )))?;
                    }
                    ProviderChunk::ReasoningArtifact(_) => {}
                    ProviderChunk::ContinuationState(state) => {
                        pending_states.push(state.clone());
                        handler.handle(RunEvent::ContinuationState(state))?;
                    }
                    ProviderChunk::Done => break,
                }
            }

            if !completed_calls.is_empty() {
                total_tool_calls += completed_calls.len();
                let assistant_message = ModelMessage::assistant(None, completed_calls.clone());
                let assistant_message_id = assistant_message.id.clone();
                session.append_assistant_message(assistant_message.clone())?;
                handler.handle(RunEvent::AssistantMessage(assistant_message))?;

                if !reasoning_buffer.is_empty() {
                    for state in &mut pending_states {
                        if state.message_id.is_none() {
                            state.message_id = Some(assistant_message_id.clone());
                        }
                    }
                }

                for state in pending_states {
                    let control = ControlEntry::ContinuationStateSaved(state);
                    session.append_control(control.clone())?;
                    handler.handle(RunEvent::Control(control))?;
                }

                let tool_ctx = ToolContext {
                    workspace_root: options.workspace_root.clone(),
                    timeout_secs: options.tool_timeout_secs,
                };
                for call in completed_calls {
                    if let Some(spec) = self.tools.spec_for(&call.name) {
                        let subject = self.tools.permission_subject(&call)?;
                        let decision = PermissionPolicy::new(&options.permission_config).decide(
                            &spec,
                            &call.name,
                            subject.clone(),
                        )?;
                        handler.handle(RunEvent::Notice(format!(
                            "permission {} subject={} mode={}",
                            call.name,
                            subject.as_deref().unwrap_or("-"),
                            decision.mode.as_str()
                        )))?;

                        match decision.mode {
                            ApprovalMode::Allow => {}
                            ApprovalMode::Ask
                                if options.interaction_mode == InteractionMode::Headless =>
                            {
                                handler.handle(RunEvent::Notice(format!(
                                    "permission ask auto-approved for {} in headless mode",
                                    call.name
                                )))?;
                            }
                            ApprovalMode::Ask => {
                                let preview = match self
                                    .tools
                                    .preview(tool_ctx.clone(), call.clone())
                                    .await
                                {
                                    Ok(preview) => preview,
                                    Err(error) => Some(ToolPreview {
                                        title: format!("Preview unavailable for {}", call.name),
                                        summary:
                                            "The tool preview could not be generated automatically."
                                                .to_owned(),
                                        body: error.to_string(),
                                        changed_files: Vec::new(),
                                        file_diffs: Vec::new(),
                                    }),
                                };
                                handler.handle(RunEvent::ToolApprovalRequested {
                                    call: call.clone(),
                                    spec: spec.clone(),
                                    preview,
                                })?;
                                match approval_handler.approve_tool_call(&call, &spec)? {
                                    ToolApproval::Approve => {
                                        handler.handle(RunEvent::ToolApprovalResolved {
                                            call_id: call.id.clone(),
                                            approved: true,
                                            reason: None,
                                        })?;
                                    }
                                    ToolApproval::Deny { reason } => {
                                        handler.handle(RunEvent::ToolApprovalResolved {
                                            call_id: call.id.clone(),
                                            approved: false,
                                            reason: Some(reason.clone()),
                                        })?;
                                        let result = crate::tool::ToolResult {
                                            call_id: call.id.clone(),
                                            tool_name: call.name.clone(),
                                            content: format!(
                                                "tool execution denied by user: {reason}"
                                            ),
                                            is_error: true,
                                            metadata: ToolResultMeta::default(),
                                        };
                                        let tool_message = ModelMessage::tool(
                                            result.call_id.clone(),
                                            result.content.clone(),
                                        );
                                        session.append_tool_message(tool_message)?;
                                        handler.handle(RunEvent::ToolResult(result))?;
                                        continue;
                                    }
                                }
                            }
                            ApprovalMode::Deny => {
                                let reason = format!(
                                    "denied by permission policy for {}",
                                    subject.as_deref().unwrap_or(&call.name)
                                );
                                handler.handle(RunEvent::ToolApprovalResolved {
                                    call_id: call.id.clone(),
                                    approved: false,
                                    reason: Some(reason.clone()),
                                })?;
                                let result = crate::tool::ToolResult {
                                    call_id: call.id.clone(),
                                    tool_name: call.name.clone(),
                                    content: reason,
                                    is_error: true,
                                    metadata: ToolResultMeta::default(),
                                };
                                let tool_message = ModelMessage::tool(
                                    result.call_id.clone(),
                                    result.content.clone(),
                                );
                                session.append_tool_message(tool_message)?;
                                handler.handle(RunEvent::ToolResult(result))?;
                                continue;
                            }
                        }
                    }

                    let result = match self.tools.execute(tool_ctx.clone(), call.clone()).await {
                        Ok(result) => result,
                        Err(error) => crate::tool::ToolResult {
                            call_id: call.id.clone(),
                            tool_name: call.name.clone(),
                            content: error.to_string(),
                            is_error: true,
                            metadata: crate::tool::ToolResultMeta::default(),
                        },
                    };
                    let tool_message =
                        ModelMessage::tool(result.call_id.clone(), result.content.clone());
                    session.append_tool_message(tool_message)?;
                    handler.handle(RunEvent::ToolResult(result))?;
                }
                continue;
            }

            let assistant_message =
                ModelMessage::assistant(Some(assistant_text.clone()), Vec::new());
            session.append_assistant_message(assistant_message.clone())?;
            handler.handle(RunEvent::AssistantMessage(assistant_message))?;

            if !pending_states.is_empty() {
                for mut state in pending_states {
                    state.message_id = Some(
                        session
                            .messages()
                            .last()
                            .map(|m| m.id.clone())
                            .unwrap_or_default(),
                    );
                    let control = ControlEntry::ContinuationStateSaved(state);
                    session.append_control(control.clone())?;
                    handler.handle(RunEvent::Control(control))?;
                }
            }

            return Ok(AgentRunResult {
                final_text: assistant_text,
                tool_calls: total_tool_calls,
            });
        }

        Err(AgentError::MaxTurnsExceeded.into())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        pin::Pin,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use anyhow::Result;
    use async_trait::async_trait;
    use futures::{Stream, stream};
    use serde_json::json;

    use crate::{
        ApprovalHandler, ApprovalMode, AutoApproveHandler, BackgroundTaskHandle,
        BackgroundTaskStatus, CompactionConfig, CompletionRequest, ControlEntry, EventHandler,
        InteractionMode, JsonlSessionStore, MemoryConfig, MessageRole, PermissionConfig, Provider,
        ProviderCapabilities, ProviderChunk, ProviderContinuationState, ReasoningEffort,
        ResponseHandle, RunEvent, Session, SessionLogEntry, Tool, ToolApproval, ToolCall,
        ToolContext, ToolRegistry, ToolResult, ToolResultMeta,
    };

    use super::{Agent, AgentRunOptions};

    struct MockProvider;

    #[async_trait]
    impl Provider for MockProvider {
        fn name(&self) -> &str {
            "mock"
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                exact_prefix_cache: false,
                reports_cache_tokens: false,
                supports_reasoning_stream: true,
                supports_tool_stream: true,
                supports_background_tasks: false,
                supports_response_handles: false,
                supports_reasoning_artifacts: false,
                supports_structured_output: false,
                supports_assistant_prefix_seed: false,
                supports_schema_constrained_tools: false,
                supports_infill_completion: false,
                supports_system_fingerprint: false,
            }
        }

        async fn stream(
            &self,
            request: CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
            let tool_used = request
                .messages
                .iter()
                .any(|message| matches!(message.role, MessageRole::Tool));
            if tool_used {
                Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderChunk::TextDelta("done".to_owned())),
                    Ok(ProviderChunk::Done),
                ])))
            } else {
                Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderChunk::ToolCallStart {
                        id: "call-1".to_owned(),
                        name: "echo".to_owned(),
                    }),
                    Ok(ProviderChunk::ToolCallArgsDelta {
                        id: "call-1".to_owned(),
                        delta: r#"{"value":"hello"}"#.to_owned(),
                    }),
                    Ok(ProviderChunk::ToolCallComplete(ToolCall {
                        id: "call-1".to_owned(),
                        name: "echo".to_owned(),
                        args_json: r#"{"value":"hello"}"#.to_owned(),
                    })),
                    Ok(ProviderChunk::Done),
                ])))
            }
        }
    }

    struct EchoTool;
    struct WriteTool {
        executed: Arc<AtomicBool>,
    }

    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> crate::ToolSpec {
            crate::ToolSpec {
                name: "echo".to_owned(),
                description: "echo".to_owned(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "value": {"type": "string"}
                    },
                    "required": ["value"]
                }),
                read_only: true,
            }
        }

        async fn execute(
            &self,
            _ctx: ToolContext,
            call_id: String,
            args: serde_json::Value,
        ) -> Result<ToolResult> {
            Ok(ToolResult {
                call_id,
                tool_name: "echo".to_owned(),
                content: args["value"].as_str().unwrap_or_default().to_owned(),
                is_error: false,
                metadata: ToolResultMeta::default(),
            })
        }
    }

    #[async_trait]
    impl Tool for WriteTool {
        fn spec(&self) -> crate::ToolSpec {
            crate::ToolSpec {
                name: "write_file".to_owned(),
                description: "write".to_owned(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"}
                    },
                    "required": ["path"]
                }),
                read_only: false,
            }
        }

        fn permission_subject(&self, args: &serde_json::Value) -> Result<Option<String>> {
            Ok(args
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned))
        }

        async fn execute(
            &self,
            _ctx: ToolContext,
            call_id: String,
            _args: serde_json::Value,
        ) -> Result<ToolResult> {
            self.executed.store(true, Ordering::SeqCst);
            Ok(ToolResult {
                call_id,
                tool_name: "write_file".to_owned(),
                content: "wrote file".to_owned(),
                is_error: false,
                metadata: ToolResultMeta::default(),
            })
        }
    }

    struct DenyWritesHandler;

    struct PanicApprovalHandler;

    impl ApprovalHandler for DenyWritesHandler {
        fn approve_tool_call(
            &mut self,
            call: &ToolCall,
            _spec: &crate::ToolSpec,
        ) -> Result<ToolApproval> {
            Ok(ToolApproval::Deny {
                reason: format!("denied {}", call.name),
            })
        }
    }

    impl ApprovalHandler for PanicApprovalHandler {
        fn approve_tool_call(
            &mut self,
            _call: &ToolCall,
            _spec: &crate::ToolSpec,
        ) -> Result<ToolApproval> {
            panic!("approval handler should not be called")
        }
    }

    #[derive(Default)]
    struct RecordingEventHandler {
        events: Vec<RunEvent>,
    }

    impl EventHandler for RecordingEventHandler {
        fn handle(&mut self, event: RunEvent) -> Result<()> {
            self.events.push(event);
            Ok(())
        }
    }

    struct StateTrackingProvider;

    #[async_trait]
    impl Provider for StateTrackingProvider {
        fn name(&self) -> &str {
            "mock-state"
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                exact_prefix_cache: false,
                reports_cache_tokens: false,
                supports_reasoning_stream: true,
                supports_tool_stream: false,
                supports_background_tasks: true,
                supports_response_handles: true,
                supports_reasoning_artifacts: false,
                supports_structured_output: false,
                supports_assistant_prefix_seed: false,
                supports_schema_constrained_tools: false,
                supports_infill_completion: false,
                supports_system_fingerprint: false,
            }
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ResponseHandle(ResponseHandle {
                    provider_name: "mock-state".to_owned(),
                    response_id: "response-1".to_owned(),
                    continuation_cursor: Some("cursor-1".to_owned()),
                })),
                Ok(ProviderChunk::BackgroundTaskAccepted(
                    BackgroundTaskHandle {
                        provider_name: "mock-state".to_owned(),
                        task_id: "task-1".to_owned(),
                        resumable: true,
                    },
                )),
                Ok(ProviderChunk::BackgroundTaskStatus(BackgroundTaskStatus {
                    provider_name: "mock-state".to_owned(),
                    task_id: "task-1".to_owned(),
                    status: "running".to_owned(),
                    metadata: BTreeMap::new(),
                })),
                Ok(ProviderChunk::ContinuationState(
                    ProviderContinuationState {
                        provider_name: "mock-state".to_owned(),
                        state_kind: "mock.cursor".to_owned(),
                        message_id: None,
                        opaque_blob: json!({"cursor":"next"}),
                    },
                )),
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ])))
        }
    }

    #[derive(Clone)]
    struct PreviousHandleRecordingProvider {
        requests: Arc<Mutex<Vec<Option<ResponseHandle>>>>,
    }

    impl PreviousHandleRecordingProvider {
        fn new() -> Self {
            Self {
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl Provider for PreviousHandleRecordingProvider {
        fn name(&self) -> &str {
            "mock-resume"
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                exact_prefix_cache: false,
                reports_cache_tokens: false,
                supports_reasoning_stream: true,
                supports_tool_stream: false,
                supports_background_tasks: false,
                supports_response_handles: true,
                supports_reasoning_artifacts: false,
                supports_structured_output: false,
                supports_assistant_prefix_seed: false,
                supports_schema_constrained_tools: false,
                supports_infill_completion: false,
                supports_system_fingerprint: false,
            }
        }

        async fn stream(
            &self,
            request: CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
            self.requests
                .lock()
                .expect("requests mutex should not be poisoned")
                .push(request.previous_response_handle);
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("resumed".to_owned())),
                Ok(ProviderChunk::Done),
            ])))
        }
    }

    struct PreviewFailingWriteTool {
        executed: Arc<AtomicBool>,
    }

    #[async_trait]
    impl Tool for PreviewFailingWriteTool {
        fn spec(&self) -> crate::ToolSpec {
            crate::ToolSpec {
                name: "write_file".to_owned(),
                description: "write".to_owned(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"}
                    },
                    "required": ["path"]
                }),
                read_only: false,
            }
        }

        async fn preview(
            &self,
            _ctx: ToolContext,
            _args: serde_json::Value,
        ) -> Result<Option<crate::ToolPreview>> {
            anyhow::bail!("preview exploded");
        }

        async fn execute(
            &self,
            _ctx: ToolContext,
            call_id: String,
            _args: serde_json::Value,
        ) -> Result<ToolResult> {
            self.executed.store(true, Ordering::SeqCst);
            Ok(ToolResult {
                call_id,
                tool_name: "write_file".to_owned(),
                content: "wrote file".to_owned(),
                is_error: false,
                metadata: ToolResultMeta::default(),
            })
        }
    }

    struct PreviewFallbackProvider;

    #[async_trait]
    impl Provider for PreviewFallbackProvider {
        fn name(&self) -> &str {
            "mock-preview"
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                exact_prefix_cache: false,
                reports_cache_tokens: false,
                supports_reasoning_stream: true,
                supports_tool_stream: true,
                supports_background_tasks: false,
                supports_response_handles: false,
                supports_reasoning_artifacts: false,
                supports_structured_output: false,
                supports_assistant_prefix_seed: false,
                supports_schema_constrained_tools: false,
                supports_infill_completion: false,
                supports_system_fingerprint: false,
            }
        }

        async fn stream(
            &self,
            request: CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
            let tool_used = request
                .messages
                .iter()
                .any(|message| matches!(message.role, MessageRole::Tool));
            if tool_used {
                Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderChunk::TextDelta("done".to_owned())),
                    Ok(ProviderChunk::Done),
                ])))
            } else {
                Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderChunk::ReasoningDelta("planning".to_owned())),
                    Ok(ProviderChunk::ToolCallStart {
                        id: "call-write-1".to_owned(),
                        name: "write_file".to_owned(),
                    }),
                    Ok(ProviderChunk::ToolCallArgsDelta {
                        id: "call-write-1".to_owned(),
                        delta: r#"{"path":"file.txt"}"#.to_owned(),
                    }),
                    Ok(ProviderChunk::ContinuationState(
                        ProviderContinuationState {
                            provider_name: "mock-preview".to_owned(),
                            state_kind: "mock.reasoning".to_owned(),
                            message_id: None,
                            opaque_blob: json!({"reasoning":"kept"}),
                        },
                    )),
                    Ok(ProviderChunk::ToolCallComplete(ToolCall {
                        id: "call-write-1".to_owned(),
                        name: "write_file".to_owned(),
                        args_json: r#"{"path":"file.txt"}"#.to_owned(),
                    })),
                    Ok(ProviderChunk::Done),
                ])))
            }
        }
    }

    #[tokio::test]
    async fn agent_runs_tool_then_answer() -> Result<()> {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        let agent = Agent::new(MockProvider, registry);
        let mut session = Session::new("mock", "mock-model");
        let mut handler = crate::event::NoopEventHandler;
        let result = agent
            .run(
                &mut session,
                "hi",
                AgentRunOptions {
                    workspace_root: std::env::temp_dir(),
                    max_turns: 4,
                    tool_timeout_secs: 5,
                    reasoning_effort: Some(ReasoningEffort::Medium),
                    traffic_partition_key: None,
                    interaction_mode: InteractionMode::Interactive,
                    permission_config: PermissionConfig::default(),
                    memory_config: MemoryConfig { enabled: false },
                    compaction_config: CompactionConfig::default(),
                },
                &mut handler,
            )
            .await?;
        assert_eq!(result.final_text, "done");
        assert_eq!(result.tool_calls, 1);
        assert!(
            session
                .messages()
                .iter()
                .any(|message| message.role == MessageRole::Tool)
        );
        Ok(())
    }

    struct WriteMockProvider;

    #[async_trait]
    impl Provider for WriteMockProvider {
        fn name(&self) -> &str {
            "mock-write"
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                exact_prefix_cache: false,
                reports_cache_tokens: false,
                supports_reasoning_stream: true,
                supports_tool_stream: true,
                supports_background_tasks: false,
                supports_response_handles: false,
                supports_reasoning_artifacts: false,
                supports_structured_output: false,
                supports_assistant_prefix_seed: false,
                supports_schema_constrained_tools: false,
                supports_infill_completion: false,
                supports_system_fingerprint: false,
            }
        }

        async fn stream(
            &self,
            request: CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
            let tool_used = request
                .messages
                .iter()
                .any(|message| matches!(message.role, MessageRole::Tool));
            if tool_used {
                Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderChunk::TextDelta("done".to_owned())),
                    Ok(ProviderChunk::Done),
                ])))
            } else {
                Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderChunk::ToolCallStart {
                        id: "call-write-1".to_owned(),
                        name: "write_file".to_owned(),
                    }),
                    Ok(ProviderChunk::ToolCallArgsDelta {
                        id: "call-write-1".to_owned(),
                        delta: r#"{"path":"file.txt"}"#.to_owned(),
                    }),
                    Ok(ProviderChunk::ToolCallComplete(ToolCall {
                        id: "call-write-1".to_owned(),
                        name: "write_file".to_owned(),
                        args_json: r#"{"path":"file.txt"}"#.to_owned(),
                    })),
                    Ok(ProviderChunk::Done),
                ])))
            }
        }
    }

    #[tokio::test]
    async fn agent_respects_denied_write_approval() -> Result<()> {
        let executed = Arc::new(AtomicBool::new(false));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(WriteTool {
            executed: Arc::clone(&executed),
        }));
        let agent = Agent::new(WriteMockProvider, registry);
        let mut session = Session::new("mock-write", "mock-model");
        let mut handler = crate::event::NoopEventHandler;
        let mut approval_handler = DenyWritesHandler;
        let result = agent
            .run_with_approval(
                &mut session,
                "write something",
                AgentRunOptions {
                    workspace_root: std::env::temp_dir(),
                    max_turns: 4,
                    tool_timeout_secs: 5,
                    reasoning_effort: Some(ReasoningEffort::Medium),
                    traffic_partition_key: None,
                    interaction_mode: InteractionMode::Interactive,
                    permission_config: PermissionConfig::default(),
                    memory_config: MemoryConfig { enabled: false },
                    compaction_config: CompactionConfig::default(),
                },
                &mut handler,
                &mut approval_handler,
            )
            .await?;
        assert_eq!(result.final_text, "done");
        assert!(!executed.load(Ordering::SeqCst));
        Ok(())
    }

    #[tokio::test]
    async fn agent_auto_allows_ask_mode_in_headless_runs() -> Result<()> {
        let executed = Arc::new(AtomicBool::new(false));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(WriteTool {
            executed: Arc::clone(&executed),
        }));
        let agent = Agent::new(WriteMockProvider, registry);
        let mut session = Session::new("mock-write", "mock-model");
        let mut handler = RecordingEventHandler::default();
        let mut approval_handler = PanicApprovalHandler;
        let result = agent
            .run_with_approval(
                &mut session,
                "write something",
                AgentRunOptions {
                    workspace_root: std::env::temp_dir(),
                    max_turns: 4,
                    tool_timeout_secs: 5,
                    reasoning_effort: Some(ReasoningEffort::Medium),
                    traffic_partition_key: None,
                    interaction_mode: InteractionMode::Headless,
                    permission_config: PermissionConfig::default(),
                    memory_config: MemoryConfig { enabled: false },
                    compaction_config: CompactionConfig::default(),
                },
                &mut handler,
                &mut approval_handler,
            )
            .await?;

        assert_eq!(result.final_text, "done");
        assert!(executed.load(Ordering::SeqCst));
        assert!(handler.events.iter().any(|event| {
            matches!(event, RunEvent::Notice(note) if note.contains("headless mode"))
        }));
        Ok(())
    }

    #[tokio::test]
    async fn agent_denies_write_when_subject_rule_matches() -> Result<()> {
        let executed = Arc::new(AtomicBool::new(false));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(WriteTool {
            executed: Arc::clone(&executed),
        }));
        let agent = Agent::new(WriteMockProvider, registry);
        let mut session = Session::new("mock-write", "mock-model");
        let mut handler = RecordingEventHandler::default();
        let mut approval_handler = PanicApprovalHandler;
        let result = agent
            .run_with_approval(
                &mut session,
                "write something",
                AgentRunOptions {
                    workspace_root: std::env::temp_dir(),
                    max_turns: 4,
                    tool_timeout_secs: 5,
                    reasoning_effort: Some(ReasoningEffort::Medium),
                    traffic_partition_key: None,
                    interaction_mode: InteractionMode::Interactive,
                    permission_config: PermissionConfig {
                        write_mode: ApprovalMode::Ask,
                        rules: vec![crate::PermissionRule {
                            tool_name: "write_file".to_owned(),
                            subject_glob: Some("file.txt".to_owned()),
                            mode: ApprovalMode::Deny,
                        }],
                    },
                    memory_config: MemoryConfig { enabled: false },
                    compaction_config: CompactionConfig::default(),
                },
                &mut handler,
                &mut approval_handler,
            )
            .await?;

        assert_eq!(result.final_text, "done");
        assert!(!executed.load(Ordering::SeqCst));
        assert!(handler.events.iter().any(|event| {
            matches!(
                event,
                RunEvent::ToolApprovalResolved { approved: false, reason: Some(reason), .. }
                    if reason.contains("permission policy")
            )
        }));
        Ok(())
    }

    #[tokio::test]
    async fn agent_tracks_response_handles_background_tasks_and_continuation_state() -> Result<()> {
        let registry = ToolRegistry::new();
        let agent = Agent::new(StateTrackingProvider, registry);
        let mut session = Session::new("mock-state", "mock-model");
        let mut handler = RecordingEventHandler::default();
        let result = agent
            .run(
                &mut session,
                "continue",
                AgentRunOptions {
                    workspace_root: std::env::temp_dir(),
                    max_turns: 1,
                    tool_timeout_secs: 5,
                    reasoning_effort: Some(ReasoningEffort::Medium),
                    traffic_partition_key: None,
                    interaction_mode: InteractionMode::Interactive,
                    permission_config: PermissionConfig::default(),
                    memory_config: MemoryConfig { enabled: false },
                    compaction_config: CompactionConfig::default(),
                },
                &mut handler,
            )
            .await?;

        assert_eq!(result.final_text, "done");
        assert!(session.entries().iter().any(|entry| {
            matches!(
                entry,
                crate::SessionLogEntry::Control(ControlEntry::ResponseHandleTracked(handle))
                    if handle.response_id == "response-1"
            )
        }));
        assert!(session.entries().iter().any(|entry| {
            matches!(
                entry,
                crate::SessionLogEntry::Control(ControlEntry::BackgroundTaskTracked(handle))
                    if handle.task_id == "task-1"
            )
        }));
        let saved_state = session.entries().iter().find_map(|entry| match entry {
            crate::SessionLogEntry::Control(ControlEntry::ContinuationStateSaved(state)) => {
                Some(state)
            }
            _ => None,
        });
        assert!(matches!(
            saved_state,
            Some(state) if state.message_id.as_deref().is_some_and(|id| !id.is_empty())
        ));
        assert!(handler.events.iter().any(|event| {
            matches!(event, RunEvent::Notice(note) if note.contains("background task task-1 status running"))
        }));
        Ok(())
    }

    #[tokio::test]
    async fn agent_restores_previous_response_handle_from_durable_control_state() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let session_path = temp.path().join("session.jsonl");
        let store = JsonlSessionStore::new(&session_path)?;
        store.append(&SessionLogEntry::Control(
            ControlEntry::ResponseHandleTracked(ResponseHandle {
                provider_name: "mock-resume".to_owned(),
                response_id: "response-old".to_owned(),
                continuation_cursor: Some("cursor-old".to_owned()),
            }),
        ))?;
        store.append(&SessionLogEntry::Control(
            ControlEntry::ResponseHandleTracked(ResponseHandle {
                provider_name: "other-provider".to_owned(),
                response_id: "response-other".to_owned(),
                continuation_cursor: None,
            }),
        ))?;
        store.append(&SessionLogEntry::Control(
            ControlEntry::ResponseHandleTracked(ResponseHandle {
                provider_name: "mock-resume".to_owned(),
                response_id: "response-new".to_owned(),
                continuation_cursor: Some("cursor-new".to_owned()),
            }),
        ))?;
        let mut session = Session::load_from_store("mock-resume", "mock-model", store)?;
        let provider = PreviousHandleRecordingProvider::new();
        let requests = Arc::clone(&provider.requests);
        let agent = Agent::new(provider, ToolRegistry::new());
        let mut handler = crate::event::NoopEventHandler;

        let result = agent
            .run(
                &mut session,
                "resume from control state",
                AgentRunOptions {
                    workspace_root: temp.path().to_path_buf(),
                    max_turns: 1,
                    tool_timeout_secs: 5,
                    reasoning_effort: Some(ReasoningEffort::Medium),
                    traffic_partition_key: None,
                    interaction_mode: InteractionMode::Interactive,
                    permission_config: PermissionConfig::default(),
                    memory_config: MemoryConfig { enabled: false },
                    compaction_config: CompactionConfig::default(),
                },
                &mut handler,
            )
            .await?;

        assert_eq!(result.final_text, "resumed");
        let seen_requests = requests
            .lock()
            .expect("requests mutex should not be poisoned");
        assert_eq!(seen_requests.len(), 1);
        assert!(matches!(
            seen_requests[0].as_ref(),
            Some(handle) if handle.provider_name == "mock-resume"
                && handle.response_id == "response-new"
                && handle.continuation_cursor.as_deref() == Some("cursor-new")
        ));
        Ok(())
    }

    #[tokio::test]
    async fn agent_uses_preview_fallback_and_binds_reasoning_state_to_tool_message() -> Result<()> {
        let executed = Arc::new(AtomicBool::new(false));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(PreviewFailingWriteTool {
            executed: Arc::clone(&executed),
        }));
        let agent = Agent::new(PreviewFallbackProvider, registry);
        let mut session = Session::new("mock-preview", "mock-model");
        let mut handler = RecordingEventHandler::default();
        let mut approval_handler = AutoApproveHandler;
        let result = agent
            .run_with_approval(
                &mut session,
                "write file",
                AgentRunOptions {
                    workspace_root: std::env::temp_dir(),
                    max_turns: 4,
                    tool_timeout_secs: 5,
                    reasoning_effort: Some(ReasoningEffort::Medium),
                    traffic_partition_key: None,
                    interaction_mode: InteractionMode::Interactive,
                    permission_config: PermissionConfig {
                        write_mode: ApprovalMode::Ask,
                        rules: Vec::new(),
                    },
                    memory_config: MemoryConfig { enabled: false },
                    compaction_config: CompactionConfig::default(),
                },
                &mut handler,
                &mut approval_handler,
            )
            .await?;

        assert_eq!(result.final_text, "done");
        assert!(executed.load(Ordering::SeqCst));
        assert!(handler.events.iter().any(|event| {
            matches!(
                event,
                RunEvent::ToolApprovalRequested { preview: Some(preview), .. }
                    if preview.title.contains("Preview unavailable for write_file")
                        && preview.body.contains("preview exploded")
            )
        }));
        assert!(handler.events.iter().any(|event| {
            matches!(event, RunEvent::ToolApprovalResolved { approved: true, .. })
        }));

        let assistant_tool_message_id = session
            .messages()
            .iter()
            .find(|message| !message.tool_calls.is_empty())
            .map(|message| message.id.clone());
        let saved_state = session.entries().iter().find_map(|entry| match entry {
            crate::SessionLogEntry::Control(ControlEntry::ContinuationStateSaved(state)) => {
                Some(state)
            }
            _ => None,
        });
        assert_eq!(
            saved_state.and_then(|state| state.message_id.clone()),
            assistant_tool_message_id
        );
        Ok(())
    }
}
