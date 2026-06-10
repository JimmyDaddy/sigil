use std::{collections::BTreeMap, time::Instant};

use anyhow::{Context, Result};
use futures::StreamExt;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{
    approval::{ApprovalHandler, AutoApproveHandler, ToolApproval},
    config::{CompactionConfig, MemoryConfig},
    event::{EventHandler, RunEvent},
    permission::{
        ApprovalMode, InteractionMode, PermissionConfig, PermissionDecision, PermissionPolicy,
    },
    provider::{ModelMessage, Provider, ProviderChunk, ProviderContinuationState, ToolCall},
    session::{
        ControlEntry, Session, ToolApprovalAuditAction, ToolApprovalEntry,
        ToolApprovalUserDecision, ToolExecutionEntry, ToolExecutionStatus, ToolSubjectAudit,
    },
    tool::{
        ToolContext, ToolDiffBudget, ToolErrorKind, ToolPreview, ToolPreviewSnapshot, ToolRegistry,
        ToolResult, ToolResultMeta, ToolResultStatus, ToolSubject, ToolSubjectScope,
    },
};

/// Runtime knobs for one agent run.
#[derive(Debug, Clone)]
pub struct AgentRunOptions {
    pub workspace_root: std::path::PathBuf,
    pub max_turns: Option<usize>,
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

    /// Returns the registered tool surface used by this agent.
    pub fn tool_registry(&self) -> &ToolRegistry {
        &self.tools
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

        let permission_policy = PermissionPolicy::new(&options.permission_config);
        let mut previous_response_handle = session.latest_response_handle(self.provider.name());
        let mut total_tool_calls = 0usize;

        let mut model_turns = 0usize;
        loop {
            if let Some(max_turns) = options.max_turns
                && model_turns >= max_turns
            {
                handler.handle(RunEvent::Notice(format!(
                    "Stopped after {model_turns} model turns: the model kept requesting tools and did not return a final answer. Send another message to continue from the recorded tool results."
                )))?;
                return Ok(AgentRunResult {
                    final_text: String::new(),
                    tool_calls: total_tool_calls,
                });
            }
            model_turns = model_turns.saturating_add(1);

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
            let mut reasoning_trace_buffer = String::new();
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
                        reasoning_trace_buffer.push_str(&delta);
                        handler.handle(RunEvent::ReasoningDelta(delta))?;
                    }
                    ProviderChunk::ReasoningSummaryDelta(delta) => {
                        reasoning_trace_buffer.push_str(&delta);
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

            append_reasoning_trace(session, &reasoning_trace_buffer)?;

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
                    let mut execution_subjects = Vec::new();
                    if let Some(spec) = self.tools.spec_for(&call.name) {
                        let subjects = match self.tools.permission_subjects(&tool_ctx, &call) {
                            Ok(subjects) => subjects,
                            Err(error) => {
                                let mut result = ToolResult::error(
                                    call.id.clone(),
                                    call.name.clone(),
                                    ToolErrorKind::InvalidInput,
                                    format!("invalid tool arguments for {}: {error}", call.name),
                                );
                                attach_tool_call_context(&mut result, &call, &[]);
                                append_tool_execution_audit(
                                    session,
                                    &call,
                                    &[],
                                    ToolExecutionStatus::Failed,
                                    None,
                                    Some(&result),
                                )?;
                                session.append_tool_message(result.to_model_message())?;
                                handler.handle(RunEvent::ToolResult(result))?;
                                continue;
                            }
                        };
                        let access = match self.tools.permission_access(&tool_ctx, &call) {
                            Ok(access) => access,
                            Err(error) => {
                                let mut result = ToolResult::error(
                                    call.id.clone(),
                                    call.name.clone(),
                                    ToolErrorKind::InvalidInput,
                                    format!("invalid tool arguments for {}: {error}", call.name),
                                );
                                attach_tool_call_context(&mut result, &call, &subjects);
                                append_tool_execution_audit(
                                    session,
                                    &call,
                                    &subjects,
                                    ToolExecutionStatus::Failed,
                                    None,
                                    Some(&result),
                                )?;
                                session.append_tool_message(result.to_model_message())?;
                                handler.handle(RunEvent::ToolResult(result))?;
                                continue;
                            }
                        };
                        let decision = permission_policy.decide_with_access(
                            &spec,
                            &call.name,
                            access,
                            subjects.clone(),
                        )?;
                        let subject_label = if decision.subjects.is_empty() {
                            "-".to_owned()
                        } else {
                            decision
                                .subjects
                                .iter()
                                .map(|subject| subject.normalized.as_str())
                                .collect::<Vec<_>>()
                                .join(",")
                        };
                        handler.handle(RunEvent::Notice(format!(
                            "permission {} subject={} mode={}",
                            call.name,
                            subject_label,
                            decision.mode.as_str()
                        )))?;
                        append_tool_approval_audit(
                            session,
                            &call,
                            &decision,
                            ToolApprovalAuditAction::PolicyEvaluated,
                            None,
                            None,
                            None,
                        )?;
                        execution_subjects = decision.subjects.clone();

                        match decision.mode {
                            ApprovalMode::Allow => {}
                            ApprovalMode::Ask
                                if options.interaction_mode == InteractionMode::Headless =>
                            {
                                let reason = format!(
                                    "tool {} requires approval in headless mode",
                                    call.name
                                );
                                append_tool_approval_audit(
                                    session,
                                    &call,
                                    &decision,
                                    ToolApprovalAuditAction::Resolved,
                                    Some(ToolApprovalUserDecision::Denied),
                                    Some(reason.clone()),
                                    None,
                                )?;
                                let mut result = ToolResult::error(
                                    call.id.clone(),
                                    call.name.clone(),
                                    ToolErrorKind::ApprovalRequired,
                                    reason,
                                );
                                attach_tool_call_context(&mut result, &call, &decision.subjects);
                                session.append_tool_message(result.to_model_message())?;
                                handler.handle(RunEvent::ToolResult(result))?;
                                continue;
                            }
                            ApprovalMode::Ask => {
                                let mut preview_error = None;
                                let preview = if has_external_subject(&decision.subjects) {
                                    Some(external_directory_preview(&call.name, &decision.subjects))
                                } else {
                                    match self.tools.preview(tool_ctx.clone(), call.clone()).await {
                                        Ok(preview) => preview,
                                        Err(error) => {
                                            let error = error.to_string();
                                            preview_error = Some(error.clone());
                                            Some(ToolPreview {
                                                title: format!(
                                                    "Preview unavailable for {}",
                                                    call.name
                                                ),
                                                summary: "The tool preview could not be generated automatically."
                                                    .to_owned(),
                                                body: error,
                                                changed_files: Vec::new(),
                                                file_diffs: Vec::new(),
                                            })
                                        }
                                    }
                                };
                                if let Some(error) = preview_error.as_ref() {
                                    append_tool_approval_audit(
                                        session,
                                        &call,
                                        &decision,
                                        ToolApprovalAuditAction::PreviewFailed,
                                        None,
                                        Some(error.clone()),
                                        None,
                                    )?;
                                }
                                let preview_hash =
                                    preview.as_ref().map(stable_json_hash).transpose()?;
                                if preview_error.is_none()
                                    && let Some(preview) = preview.as_ref()
                                {
                                    let control = ControlEntry::ToolPreviewCaptured(
                                        ToolPreviewSnapshot::from_preview(
                                            call.id.clone(),
                                            call.name.clone(),
                                            preview,
                                            ToolDiffBudget::default(),
                                            preview_hash.clone(),
                                        ),
                                    );
                                    session.append_control(control.clone())?;
                                    handler.handle(RunEvent::Control(control))?;
                                }
                                append_tool_approval_audit(
                                    session,
                                    &call,
                                    &decision,
                                    ToolApprovalAuditAction::Requested,
                                    None,
                                    None,
                                    preview_hash.clone(),
                                )?;
                                handler.handle(RunEvent::ToolApprovalRequested {
                                    call: call.clone(),
                                    spec: spec.clone(),
                                    subjects: decision.subjects.clone(),
                                    preview,
                                })?;
                                match approval_handler.approve_tool_call(&call, &spec)? {
                                    ToolApproval::Approve => {
                                        append_tool_approval_audit(
                                            session,
                                            &call,
                                            &decision,
                                            ToolApprovalAuditAction::Resolved,
                                            Some(ToolApprovalUserDecision::Approved),
                                            None,
                                            preview_hash,
                                        )?;
                                        handler.handle(RunEvent::ToolApprovalResolved {
                                            call_id: call.id.clone(),
                                            approved: true,
                                            reason: None,
                                        })?;
                                    }
                                    ToolApproval::Deny { reason } => {
                                        append_tool_approval_audit(
                                            session,
                                            &call,
                                            &decision,
                                            ToolApprovalAuditAction::Resolved,
                                            Some(ToolApprovalUserDecision::Denied),
                                            Some(reason.clone()),
                                            preview_hash,
                                        )?;
                                        handler.handle(RunEvent::ToolApprovalResolved {
                                            call_id: call.id.clone(),
                                            approved: false,
                                            reason: Some(reason.clone()),
                                        })?;
                                        let mut result = ToolResult::error(
                                            call.id.clone(),
                                            call.name.clone(),
                                            ToolErrorKind::ApprovalDenied,
                                            format!("tool execution denied by user: {reason}"),
                                        );
                                        attach_tool_call_context(
                                            &mut result,
                                            &call,
                                            &decision.subjects,
                                        );
                                        session.append_tool_message(result.to_model_message())?;
                                        handler.handle(RunEvent::ToolResult(result))?;
                                        continue;
                                    }
                                }
                            }
                            ApprovalMode::Deny => {
                                let (error_kind, reason) = if decision.external_directory_required {
                                    (
                                        ToolErrorKind::ExternalDirectoryRequired,
                                        format!(
                                            "external directory access requires permission.external_directory.enabled for {}",
                                            if subject_label == "-" {
                                                call.name.as_str()
                                            } else {
                                                subject_label.as_str()
                                            }
                                        ),
                                    )
                                } else {
                                    (
                                        ToolErrorKind::PermissionDenied,
                                        format!(
                                            "denied by permission policy for {}",
                                            if subject_label == "-" {
                                                call.name.as_str()
                                            } else {
                                                subject_label.as_str()
                                            }
                                        ),
                                    )
                                };
                                append_tool_approval_audit(
                                    session,
                                    &call,
                                    &decision,
                                    ToolApprovalAuditAction::Resolved,
                                    Some(ToolApprovalUserDecision::Denied),
                                    Some(reason.clone()),
                                    None,
                                )?;
                                handler.handle(RunEvent::ToolApprovalResolved {
                                    call_id: call.id.clone(),
                                    approved: false,
                                    reason: Some(reason.clone()),
                                })?;
                                let mut result = ToolResult::error(
                                    call.id.clone(),
                                    call.name.clone(),
                                    error_kind,
                                    reason,
                                );
                                attach_tool_call_context(&mut result, &call, &decision.subjects);
                                session.append_tool_message(result.to_model_message())?;
                                handler.handle(RunEvent::ToolResult(result))?;
                                continue;
                            }
                        }
                    }

                    append_tool_execution_audit(
                        session,
                        &call,
                        &execution_subjects,
                        ToolExecutionStatus::Started,
                        None,
                        None,
                    )?;
                    let execution_started = Instant::now();
                    let mut result = match self.tools.execute(tool_ctx.clone(), call.clone()).await
                    {
                        Ok(result) => result,
                        Err(error) => ToolResult::error(
                            call.id.clone(),
                            call.name.clone(),
                            ToolErrorKind::Internal,
                            error.to_string(),
                        ),
                    };
                    attach_tool_call_context(&mut result, &call, &execution_subjects);
                    let duration_ms = Some(duration_ms(execution_started));
                    let status = if result.is_error() {
                        ToolExecutionStatus::Failed
                    } else {
                        ToolExecutionStatus::Completed
                    };
                    append_tool_execution_audit(
                        session,
                        &call,
                        &execution_subjects,
                        status,
                        duration_ms,
                        Some(&result),
                    )?;
                    session.append_tool_message(result.to_model_message())?;
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
    }
}

fn has_external_subject(subjects: &[ToolSubject]) -> bool {
    subjects
        .iter()
        .any(|subject| subject.scope == ToolSubjectScope::External)
}

fn append_reasoning_trace(session: &mut Session, trace: &str) -> Result<()> {
    if trace.is_empty() {
        return Ok(());
    }
    session.append_control(reasoning_trace_note(trace.to_owned()))
}

fn reasoning_trace_note(trace: String) -> ControlEntry {
    let mut data = Map::new();
    data.insert("text".to_owned(), Value::String(trace));
    ControlEntry::Note {
        kind: "reasoning_trace".to_owned(),
        data: Value::Object(data),
    }
}

fn attach_tool_call_context(result: &mut ToolResult, call: &ToolCall, subjects: &[ToolSubject]) {
    let Some(context) = tool_call_context(call, subjects) else {
        return;
    };
    match &mut result.metadata.details {
        Value::Object(details) => {
            details.insert("call".to_owned(), context);
        }
        Value::Null => {
            let mut details = Map::new();
            details.insert("call".to_owned(), context);
            result.metadata.details = Value::Object(details);
        }
        existing => {
            let previous = std::mem::replace(existing, Value::Null);
            let mut details = Map::new();
            details.insert("call".to_owned(), context);
            details.insert("tool".to_owned(), previous);
            *existing = Value::Object(details);
        }
    }
}

fn tool_call_context(call: &ToolCall, subjects: &[ToolSubject]) -> Option<Value> {
    let args = serde_json::from_str::<Value>(&call.args_json).ok();
    let object = args.as_ref().and_then(Value::as_object);
    let mut context = Map::new();
    let mut summary_parts = Vec::new();

    if let Some(command) = object
        .and_then(|object| object.get("command"))
        .and_then(Value::as_str)
    {
        let command = truncate_context_value(command);
        context.insert("command".to_owned(), Value::String(command.clone()));
        summary_parts.push(format!("command={command}"));
    }
    if let Some(path) = object
        .and_then(|object| object.get("path"))
        .and_then(Value::as_str)
    {
        let path = truncate_context_value(path);
        context.insert("path".to_owned(), Value::String(path.clone()));
        summary_parts.push(format!("path={path}"));
    }
    if let Some(pattern) = object
        .and_then(|object| object.get("pattern"))
        .and_then(Value::as_str)
    {
        let pattern = truncate_context_value(pattern);
        context.insert("pattern".to_owned(), Value::String(pattern.clone()));
        summary_parts.push(format!("pattern={pattern}"));
    }

    let subject_labels = subjects
        .iter()
        .take(6)
        .map(tool_subject_context_label)
        .collect::<Vec<_>>();
    if !subject_labels.is_empty() {
        context.insert(
            "subjects".to_owned(),
            Value::Array(subject_labels.iter().cloned().map(Value::String).collect()),
        );
        if summary_parts.is_empty() {
            summary_parts.push(format!("subject={}", subject_labels.join(",")));
        }
    }

    if !summary_parts.is_empty() {
        context.insert(
            "summary".to_owned(),
            Value::String(truncate_context_value(&summary_parts.join(" "))),
        );
    }

    (!context.is_empty()).then_some(Value::Object(context))
}

fn tool_subject_context_label(subject: &ToolSubject) -> String {
    let target = subject
        .canonical_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| subject.normalized.clone());
    truncate_context_value(&format!(
        "{}:{}:{}",
        subject.scope.as_str(),
        subject.kind.as_str(),
        target
    ))
}

fn truncate_context_value(value: &str) -> String {
    const MAX_CHARS: usize = 180;
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_CHARS {
        return normalized;
    }
    let truncated = normalized.chars().take(MAX_CHARS).collect::<String>();
    format!("{truncated}...")
}

fn external_directory_preview(tool_name: &str, subjects: &[ToolSubject]) -> ToolPreview {
    let external_subjects = subjects
        .iter()
        .filter(|subject| subject.scope == ToolSubjectScope::External)
        .map(|subject| {
            subject
                .canonical_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| subject.normalized.clone())
        })
        .collect::<Vec<_>>();
    let body = if external_subjects.is_empty() {
        "No external path subjects were reported.".to_owned()
    } else {
        external_subjects.join("\n")
    };
    ToolPreview {
        title: format!("External directory access for {tool_name}"),
        summary: "This tool call touches a path outside the workspace.".to_owned(),
        body,
        changed_files: Vec::new(),
        file_diffs: Vec::new(),
    }
}

fn append_tool_approval_audit(
    session: &mut Session,
    call: &ToolCall,
    decision: &PermissionDecision,
    action: ToolApprovalAuditAction,
    user_decision: Option<ToolApprovalUserDecision>,
    reason: Option<String>,
    preview_hash: Option<String>,
) -> Result<()> {
    session.append_control(ControlEntry::ToolApproval(ToolApprovalEntry {
        action,
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        access: decision.access,
        subjects: audit_subjects(&decision.subjects),
        policy_decision: decision.mode,
        external_directory_required: decision.external_directory_required,
        user_decision,
        reason,
        preview_hash,
    }))
}

fn append_tool_execution_audit(
    session: &mut Session,
    call: &ToolCall,
    subjects: &[ToolSubject],
    status: ToolExecutionStatus,
    duration_ms: Option<u64>,
    result: Option<&ToolResult>,
) -> Result<()> {
    let (changed_files, metadata, error, model_content_hash) = if let Some(result) = result {
        let error = match &result.status {
            ToolResultStatus::Ok => None,
            ToolResultStatus::Error(error) => Some(error.clone()),
        };
        (
            result.metadata.changed_files.clone(),
            result.metadata.clone(),
            error,
            Some(stable_text_hash(&result.to_model_content())),
        )
    } else {
        let mut metadata = ToolResultMeta::default();
        if let Some(context) = tool_call_context(call, subjects) {
            let mut details = Map::new();
            details.insert("call".to_owned(), context);
            metadata.details = Value::Object(details);
        }
        (Vec::new(), metadata, None, None)
    };

    session.append_control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        status,
        duration_ms,
        subjects: audit_subjects(subjects),
        changed_files,
        metadata,
        error,
        model_content_hash,
    })))
}

fn audit_subjects(subjects: &[ToolSubject]) -> Vec<ToolSubjectAudit> {
    subjects.iter().map(ToolSubjectAudit::from).collect()
}

fn stable_json_hash<T: serde::Serialize>(value: &T) -> Result<String> {
    let serialized = serde_json::to_string(value).context("failed to serialize audit payload")?;
    Ok(stable_text_hash(&serialized))
}

fn stable_text_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}")
}

fn duration_ms(started_at: Instant) -> u64 {
    started_at
        .elapsed()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "tests/agent_tests.rs"]
mod tests;
