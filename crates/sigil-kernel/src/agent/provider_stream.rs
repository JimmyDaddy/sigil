use std::collections::BTreeMap;

use anyhow::{Context, Result};
use futures::StreamExt;

use crate::{
    event::{EventHandler, RunEvent},
    provider::{
        CompletionRequest, Provider, ProviderChunk, ProviderContinuationState, ResponseHandle,
        ToolCall,
    },
    session::{ControlEntry, Session},
};

use super::run_lifecycle::append_failed_run_lifecycle_events;

pub(super) struct ProviderTurnOutput {
    pub(super) assistant_text: String,
    pub(super) reasoning_trace: String,
    pub(super) completed_calls: Vec<ToolCall>,
    pub(super) pending_states: Vec<ProviderContinuationState>,
}

pub(super) async fn collect_provider_turn<H>(
    provider: &dyn Provider,
    session: &mut Session,
    request: CompletionRequest,
    previous_response_handle: &mut Option<ResponseHandle>,
    total_tool_calls: usize,
    handler: &mut H,
    cancellation: Option<&crate::RunCancellationHandle>,
) -> Result<ProviderTurnOutput>
where
    H: EventHandler + Send,
{
    let stream_result = match cancellation {
        Some(cancellation) => tokio::select! {
            biased;
            _ = cancellation.cancelled() => anyhow::bail!("run cancellation requested during provider connect"),
            result = provider.stream(request) => result,
        },
        None => provider.stream(request).await,
    };
    let mut stream = match stream_result {
        Ok(stream) => stream,
        Err(error) => {
            let error_message = format!("{error:#}");
            append_failed_run_lifecycle_events(
                session,
                "provider_request_error",
                total_tool_calls,
                &error_message,
            )?;
            return Err(error);
        }
    };
    let mut assistant_text = String::new();
    let mut reasoning_trace_buffer = String::new();
    let mut tool_parts: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut completed_calls: Vec<ToolCall> = Vec::new();
    let mut pending_states: Vec<ProviderContinuationState> = Vec::new();

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
        let chunk = match chunk.context("provider stream failed") {
            Ok(chunk) => chunk,
            Err(error) => {
                let error_message = format!("{error:#}");
                append_failed_run_lifecycle_events(
                    session,
                    "provider_stream_error",
                    total_tool_calls,
                    &error_message,
                )?;
                return Err(error);
            }
        };
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
            ProviderChunk::Done => break,
        }
    }

    Ok(ProviderTurnOutput {
        assistant_text,
        reasoning_trace: reasoning_trace_buffer,
        completed_calls,
        pending_states,
    })
}
