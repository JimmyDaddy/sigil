use std::collections::BTreeMap;

use anyhow::{Context, Result};
use futures::StreamExt;

use crate::{
    MAX_PROVIDER_TURN_TOOL_ARGS_BYTES, MAX_PROVIDER_TURN_TOOL_CALLS, MAX_STREAMED_TOOL_ARGS_BYTES,
    ToolCallPersistenceProjection,
    event::{EventHandler, RunEvent},
    provider::{
        CompletionRequest, Provider, ProviderChunk, ProviderContinuationState, ResponseHandle,
        ToolCall,
    },
    session::{ControlEntry, Session},
};

pub(super) struct ProviderTurnOutput {
    pub(super) assistant_text: String,
    pub(super) reasoning_trace: String,
    pub(super) completed_calls: Vec<ToolCallPersistenceProjection>,
    pub(super) pending_states: Vec<ProviderContinuationState>,
    pub(super) hosted_finalized: Option<crate::FinalizedHostedTurn>,
}

pub(super) async fn collect_provider_turn<H>(
    provider: &dyn Provider,
    session: &mut Session,
    request: CompletionRequest,
    previous_response_handle: &mut Option<ResponseHandle>,
    _total_tool_calls: usize,
    handler: &mut H,
    cancellation: Option<&crate::RunCancellationHandle>,
    hosted_processor: Option<&std::sync::Arc<dyn crate::HostedEvidenceProcessor>>,
) -> Result<ProviderTurnOutput>
where
    H: EventHandler + Send,
{
    let hosted_enabled = !request.hosted_tools.is_empty();
    if hosted_enabled && hosted_processor.is_none() {
        return Err(crate::HostedTurnError::MissingProcessor.into());
    }
    if hosted_enabled
        && !provider
            .hosted_web_search_capability(&request.model_name)
            .is_supported()
    {
        anyhow::bail!("provider model does not support hosted web search");
    }
    for hosted_tool in &request.hosted_tools {
        hosted_tool.validate()?;
    }
    let hosted_context = crate::HostedFinalizationContext {
        session_scope_id: session.session_scope_id().to_owned(),
        provider_name: request.provider_name.clone(),
        model_name: request.model_name.clone(),
    };
    let stream_result = match cancellation {
        Some(cancellation) => tokio::select! {
            biased;
            _ = cancellation.cancelled() => anyhow::bail!("run cancellation requested during provider connect"),
            result = provider.stream(request) => result,
        },
        None => provider.stream(request).await,
    };
    let mut stream = stream_result?;
    if hosted_enabled {
        return collect_hosted_provider_turn(
            &mut stream,
            session,
            previous_response_handle,
            handler,
            cancellation,
            hosted_processor.ok_or(crate::HostedTurnError::MissingProcessor)?,
            hosted_context,
        )
        .await;
    }
    let mut assistant_text = String::new();
    let mut reasoning_trace_buffer = String::new();
    let mut tool_parts: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut completed_calls: Vec<ToolCallPersistenceProjection> = Vec::new();
    let mut pending_states: Vec<ProviderContinuationState> = Vec::new();
    let mut total_tool_args_bytes = 0usize;
    let mut completed_call_ids = std::collections::BTreeSet::new();

    loop {
        let next = match cancellation {
            Some(cancellation) => tokio::select! {
                biased;
                _ = cancellation.cancelled() => anyhow::bail!("run cancellation requested during provider stream"),
                chunk = stream.next() => chunk,
            },
            None => stream.next().await,
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = chunk.context("provider stream failed")?;
        match chunk {
            ProviderChunk::TextDelta(delta) => {
                assistant_text.push_str(&delta);
                handler.handle(RunEvent::TextDelta(delta))?;
            }
            ProviderChunk::ReasoningDelta(delta) => {
                reasoning_trace_buffer.push_str(&delta);
                handler.handle(RunEvent::ReasoningDelta(delta))?;
            }
            ProviderChunk::ReasoningSummaryDelta(delta) => {
                reasoning_trace_buffer.push_str(&delta);
                handler.handle(RunEvent::ReasoningDelta(delta))?;
            }
            ProviderChunk::ToolCallStart { id, name } => {
                validate_streamed_tool_identity(&id, &name)?;
                if tool_parts.len() >= MAX_PROVIDER_TURN_TOOL_CALLS && !tool_parts.contains_key(&id)
                {
                    anyhow::bail!(
                        "tool_call_stream_invalid: provider turn exceeded {MAX_PROVIDER_TURN_TOOL_CALLS} tool calls"
                    );
                }
                if tool_parts.contains_key(&id) || completed_call_ids.contains(&id) {
                    anyhow::bail!("tool_call_stream_invalid: provider reused a tool-call id");
                }
                tool_parts.insert(id.clone(), (name.clone(), String::new()));
                handler.handle(RunEvent::ToolCallStarted(ToolCall {
                    id,
                    name: crate::safe_persistence_text(&name),
                    args_json: String::new(),
                }))?;
            }
            ProviderChunk::ToolCallArgsDelta { id, delta } => {
                crate::persistence::validate_tool_call_id(&id)?;
                let Some((_, current_args)) = tool_parts.get(&id) else {
                    anyhow::bail!(
                        "tool_call_stream_invalid: arguments arrived before a matching tool-call start"
                    );
                };
                let next_call_bytes = current_args.len().saturating_add(delta.len());
                let next_total_bytes = total_tool_args_bytes.saturating_add(delta.len());
                if next_call_bytes > MAX_STREAMED_TOOL_ARGS_BYTES {
                    tool_parts.values_mut().for_each(|(_, args)| args.clear());
                    tool_parts.clear();
                    anyhow::bail!(
                        "tool_args_too_large: observed at least {next_call_bytes} bytes, limit {MAX_STREAMED_TOOL_ARGS_BYTES}"
                    );
                }
                if next_total_bytes > MAX_PROVIDER_TURN_TOOL_ARGS_BYTES {
                    tool_parts.values_mut().for_each(|(_, args)| args.clear());
                    tool_parts.clear();
                    anyhow::bail!(
                        "tool_args_too_large: provider turn observed at least {next_total_bytes} bytes, limit {MAX_PROVIDER_TURN_TOOL_ARGS_BYTES}"
                    );
                }
                let Some((_, args_json)) = tool_parts.get_mut(&id) else {
                    anyhow::bail!(
                        "tool_call_stream_invalid: tool-call state disappeared before append"
                    );
                };
                args_json.push_str(&delta);
                total_tool_args_bytes = next_total_bytes;
                handler.handle(RunEvent::ToolCallArgsDelta {
                    id,
                    delta: format!("[{} argument bytes buffered]", args_json.len()),
                })?;
            }
            ProviderChunk::ToolCallComplete(call) => {
                if completed_calls.len() >= MAX_PROVIDER_TURN_TOOL_CALLS {
                    anyhow::bail!(
                        "tool_call_stream_invalid: provider turn exceeded {MAX_PROVIDER_TURN_TOOL_CALLS} completed tool calls"
                    );
                }
                validate_streamed_tool_identity(&call.id, &call.name)?;
                if !completed_call_ids.insert(call.id.clone()) {
                    anyhow::bail!(
                        "tool_call_stream_invalid: provider reused a completed tool-call id"
                    );
                }
                if let Some((streamed_name, streamed_args)) = tool_parts.remove(&call.id) {
                    if streamed_name != call.name || streamed_args != call.args_json {
                        anyhow::bail!(
                            "tool_call_stream_invalid: completed tool call conflicts with streamed identity or arguments"
                        );
                    }
                } else {
                    let next_total_bytes =
                        total_tool_args_bytes.saturating_add(call.args_json.len());
                    if next_total_bytes > MAX_PROVIDER_TURN_TOOL_ARGS_BYTES {
                        anyhow::bail!(
                            "tool_args_too_large: provider turn observed at least {next_total_bytes} bytes, limit {MAX_PROVIDER_TURN_TOOL_ARGS_BYTES}"
                        );
                    }
                    total_tool_args_bytes = next_total_bytes;
                }
                let projection = crate::project_tool_call_for_persistence(call)?;
                handler.handle(RunEvent::ToolCallCompleted(projection.durable_call.clone()))?;
                completed_calls.push(projection);
            }
            ProviderChunk::Usage(usage) => {
                session.stats_mut().apply_usage(&usage);
                session.append_control(ControlEntry::UsageSnapshot(usage.clone()))?;
                handler.handle(RunEvent::Usage(usage))?;
            }
            ProviderChunk::ResponseHandle(handle) => {
                *previous_response_handle = Some(handle.clone());
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
            ProviderChunk::ToolCallStreamError(error) => return Err(error.into()),
            ProviderChunk::HostedToolStarted { .. }
            | ProviderChunk::HostedEvidence { .. }
            | ProviderChunk::HostedToolFailed { .. }
            | ProviderChunk::HostedRequestUsage { .. } => {
                anyhow::bail!("provider emitted hosted evidence for a non-hosted request")
            }
            ProviderChunk::Done => break,
        }
    }

    if !tool_parts.is_empty() {
        tool_parts.values_mut().for_each(|(_, args)| args.clear());
        anyhow::bail!("tool_call_stream_invalid: provider ended with incomplete tool calls");
    }

    Ok(ProviderTurnOutput {
        assistant_text,
        reasoning_trace: reasoning_trace_buffer,
        completed_calls,
        pending_states,
        hosted_finalized: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn collect_hosted_provider_turn<H>(
    stream: &mut std::pin::Pin<
        Box<dyn futures::Stream<Item = anyhow::Result<ProviderChunk>> + Send>,
    >,
    session: &mut Session,
    previous_response_handle: &mut Option<ResponseHandle>,
    handler: &mut H,
    cancellation: Option<&crate::RunCancellationHandle>,
    processor: &std::sync::Arc<dyn crate::HostedEvidenceProcessor>,
    context: crate::HostedFinalizationContext,
) -> Result<ProviderTurnOutput>
where
    H: EventHandler + Send,
{
    let mut buffer = crate::HostedTurnBuffer::new(crate::HostedTurnBufferLimits::default());
    let mut tool_parts: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut completed_calls = Vec::new();
    let mut completed_call_ids = std::collections::BTreeSet::new();
    let mut total_tool_args_bytes = 0usize;
    loop {
        let next = match cancellation {
            Some(cancellation) => tokio::select! {
                biased;
                _ = cancellation.cancelled() => anyhow::bail!("hosted provider turn cancelled before safe finalization"),
                chunk = stream.next() => chunk,
            },
            None => stream.next().await,
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = chunk.context("hosted provider stream failed before safe finalization")?;
        if matches!(chunk, ProviderChunk::Done) {
            break;
        }
        match chunk {
            ProviderChunk::ToolCallStart { id, name } => {
                validate_streamed_tool_identity(&id, &name)?;
                if tool_parts.len() >= MAX_PROVIDER_TURN_TOOL_CALLS
                    || tool_parts.contains_key(&id)
                    || completed_call_ids.contains(&id)
                {
                    anyhow::bail!("tool_call_stream_invalid: invalid hosted mixed-tool identity");
                }
                tool_parts.insert(id, (name, String::new()));
            }
            ProviderChunk::ToolCallArgsDelta { id, delta } => {
                crate::persistence::validate_tool_call_id(&id)?;
                let Some((_, args)) = tool_parts.get_mut(&id) else {
                    anyhow::bail!(
                        "tool_call_stream_invalid: hosted mixed-tool args arrived before start"
                    );
                };
                let next_call_bytes = args.len().saturating_add(delta.len());
                let next_total_bytes = total_tool_args_bytes.saturating_add(delta.len());
                if next_call_bytes > MAX_STREAMED_TOOL_ARGS_BYTES
                    || next_total_bytes > MAX_PROVIDER_TURN_TOOL_ARGS_BYTES
                {
                    anyhow::bail!(
                        "tool_args_too_large: hosted mixed-tool arguments exceeded limit"
                    );
                }
                args.push_str(&delta);
                total_tool_args_bytes = next_total_bytes;
            }
            ProviderChunk::ToolCallComplete(call) => {
                validate_streamed_tool_identity(&call.id, &call.name)?;
                if completed_calls.len() >= MAX_PROVIDER_TURN_TOOL_CALLS
                    || !completed_call_ids.insert(call.id.clone())
                {
                    anyhow::bail!("tool_call_stream_invalid: invalid hosted mixed-tool completion");
                }
                if let Some((streamed_name, streamed_args)) = tool_parts.remove(&call.id) {
                    if streamed_name != call.name || streamed_args != call.args_json {
                        anyhow::bail!(
                            "tool_call_stream_invalid: hosted mixed-tool completion drifted"
                        );
                    }
                } else {
                    let next_total_bytes =
                        total_tool_args_bytes.saturating_add(call.args_json.len());
                    if next_total_bytes > MAX_PROVIDER_TURN_TOOL_ARGS_BYTES {
                        anyhow::bail!(
                            "tool_args_too_large: hosted mixed-tool arguments exceeded limit"
                        );
                    }
                    total_tool_args_bytes = next_total_bytes;
                }
                completed_calls.push(crate::project_tool_call_for_persistence(call)?);
            }
            ProviderChunk::ToolCallStreamError(error) => return Err(error.into()),
            chunk => buffer.push(chunk)?,
        }
    }
    if !tool_parts.is_empty() {
        anyhow::bail!("tool_call_stream_invalid: hosted turn ended with incomplete client tools");
    }
    if buffer.provider_failed() {
        return Err(crate::HostedTurnError::ProviderFailed.into());
    }
    if cancellation.is_some_and(crate::RunCancellationHandle::is_cancel_requested) {
        anyhow::bail!("hosted provider turn cancelled before safe finalization");
    }
    let finalized = processor
        .finalize(context, &buffer)
        .await
        .map_err(|_| crate::HostedTurnError::FinalizationFailed)?;

    for usage in buffer.usages() {
        session.stats_mut().apply_usage(usage);
        session.append_control(ControlEntry::UsageSnapshot(usage.clone()))?;
        handler.handle(RunEvent::Usage(usage.clone()))?;
    }
    for handle in buffer.response_handles() {
        *previous_response_handle = Some(handle.clone());
        let control = ControlEntry::ResponseHandleTracked(handle.clone());
        session.append_control(control.clone())?;
        handler.handle(RunEvent::Control(control))?;
    }
    for handle in buffer.background_accepted() {
        let control = ControlEntry::BackgroundTaskTracked(handle.clone());
        session.append_control(control.clone())?;
        handler.handle(RunEvent::Control(control))?;
    }
    for status in buffer.background_statuses() {
        handler.handle(RunEvent::Notice(format!(
            "background task {} status {}",
            status.task_id, status.status
        )))?;
    }
    let pending_states = buffer.continuation_states().to_vec();
    for state in &pending_states {
        handler.handle(RunEvent::ContinuationState(state.clone()))?;
    }
    if !finalized.reasoning_trace.is_empty() {
        handler.handle(RunEvent::ReasoningDelta(finalized.reasoning_trace.clone()))?;
    }
    if !finalized.assistant_text.is_empty() {
        handler.handle(RunEvent::TextDelta(finalized.assistant_text.clone()))?;
    }
    for projection in &completed_calls {
        handler.handle(RunEvent::ToolCallStarted(ToolCall {
            id: projection.durable_call.id.clone(),
            name: projection.durable_call.name.clone(),
            args_json: String::new(),
        }))?;
        handler.handle(RunEvent::ToolCallCompleted(projection.durable_call.clone()))?;
    }

    Ok(ProviderTurnOutput {
        assistant_text: finalized.assistant_text.clone(),
        reasoning_trace: finalized.reasoning_trace.clone(),
        completed_calls,
        pending_states,
        hosted_finalized: Some(finalized),
    })
}

fn validate_streamed_tool_identity(id: &str, name: &str) -> Result<()> {
    crate::persistence::validate_tool_call_id(id)?;
    crate::persistence::validate_tool_call_name(name)?;
    Ok(())
}
