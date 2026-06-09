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
                        let subject = match self.tools.permission_subject(&call) {
                            Ok(subject) => subject,
                            Err(error) => {
                                let result = crate::tool::ToolResult {
                                    call_id: call.id.clone(),
                                    tool_name: call.name.clone(),
                                    content: format!(
                                        "invalid tool arguments for {}: {error}",
                                        call.name
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
                        };
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
#[path = "tests/agent_tests.rs"]
mod tests;
