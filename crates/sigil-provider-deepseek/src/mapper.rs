use std::collections::BTreeSet;

use anyhow::Result;
use tracing::debug;

use sigil_kernel::{
    CacheTokenCountV1, CacheUsageV1, ProviderChunk, ProviderProtocolViolation,
    ProviderStreamEndedUnexpectedly, ToolCallCompletionIdPolicy, ToolCallStreamAccumulator,
    UsageStats,
};

use crate::{
    models::{DeepSeekStreamEnvelope, DeepSeekToolCallDelta},
    reasoning::DeepSeekReasoningReplayPayload,
};

pub struct StreamMapper {
    tool_parts: ToolCallStreamAccumulator,
    tool_indices: BTreeSet<usize>,
    argument_fragments: usize,
    argument_bytes: usize,
    completed_calls: usize,
    finish_reason: Option<&'static str>,
    reasoning_buffer: String,
    text_protocol_tail: String,
}

// DeepSeek occasionally emits its internal DSML tool-call syntax as ordinary assistant text
// instead of using the structured `delta.tool_calls` stream. Treating that text as a successful
// final answer would let a task appear complete although no requested tool ever ran.
const NATIVE_TOOL_PROTOCOL_SENTINEL: &str = "<｜｜DSML｜｜tool_calls>";

impl StreamMapper {
    pub fn new(_model: impl Into<String>) -> Self {
        Self {
            tool_parts: ToolCallStreamAccumulator::new(),
            tool_indices: BTreeSet::new(),
            argument_fragments: 0,
            argument_bytes: 0,
            completed_calls: 0,
            finish_reason: None,
            reasoning_buffer: String::new(),
            text_protocol_tail: String::new(),
        }
    }
}

impl StreamMapper {
    pub fn map_envelope(&mut self, envelope: DeepSeekStreamEnvelope) -> Result<Vec<ProviderChunk>> {
        let mut chunks = Vec::new();
        if let Some(usage) = envelope.usage {
            let cache_usage = match (
                usage.prompt_cache_hit_tokens,
                usage.prompt_cache_miss_tokens,
            ) {
                (None, None) => None,
                (read, uncached) => Some(CacheUsageV1 {
                    schema_version: CacheUsageV1::SCHEMA_VERSION,
                    read: read.map(CacheTokenCountV1::provider_reported),
                    write: None,
                    uncached: uncached.map(CacheTokenCountV1::provider_reported),
                    local_layout_mutation: None,
                    provider_miss_without_local_mutation: false,
                }),
            };
            chunks.push(ProviderChunk::Usage(UsageStats {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                cache_hit_tokens: usage.prompt_cache_hit_tokens.unwrap_or_default(),
                cache_miss_tokens: usage.prompt_cache_miss_tokens.unwrap_or_default(),
                input_cost: 0.0,
                output_cost: 0.0,
                cache_savings: 0.0,
                system_fingerprint: envelope.system_fingerprint.clone(),
                cache_usage,
                pricing_snapshot: None,
            }));
        }
        for choice in envelope.choices {
            if self.finish_reason.is_some() {
                return Err(
                    self.stream_error("provider emitted a choice after the terminal finish reason")
                );
            }
            if let Some(reason) = choice.finish_reason.as_deref() {
                self.finish_reason = Some(match reason {
                    "tool_calls" => "tool_calls",
                    "stop" => "stop",
                    "length" => "length",
                    "content_filter" => "content_filter",
                    _ => "other",
                });
            }
            if let Some(content) = choice.delta.content {
                self.reject_unstructured_native_tool_protocol(&content)?;
                chunks.push(ProviderChunk::TextDelta(content));
            }
            if let Some(reasoning_content) = choice.delta.reasoning_content {
                self.reasoning_buffer.push_str(&reasoning_content);
                chunks.push(ProviderChunk::ReasoningDelta(reasoning_content));
            }
            if let Some(tool_calls) = choice.delta.tool_calls {
                for tool_call in tool_calls {
                    self.map_tool_delta(&mut chunks, tool_call)?;
                }
            }
            if self
                .finish_reason
                .is_some_and(|reason| !matches!(reason, "tool_calls" | "stop"))
            {
                if self.finish_reason == Some("length") && !self.has_tool_calls() {
                    return Err(ProviderStreamEndedUnexpectedly.into());
                }
                return Err(
                    self.stream_error("provider response ended without a successful finish reason")
                );
            }
            if matches!(choice.finish_reason.as_deref(), Some("tool_calls")) {
                self.tool_parts.complete_open_calls(
                    &mut chunks,
                    ToolCallCompletionIdPolicy::RequireProviderId,
                );
                self.completed_calls = chunks
                    .iter()
                    .filter(|chunk| matches!(chunk, ProviderChunk::ToolCallComplete(_)))
                    .count();
                let completed_argument_bytes = chunks
                    .iter()
                    .filter_map(|chunk| match chunk {
                        ProviderChunk::ToolCallComplete(call) => Some(call.args_json.len()),
                        _ => None,
                    })
                    .fold(0usize, usize::saturating_add);
                if self.completed_calls != self.tool_indices.len()
                    || self.completed_calls == 0
                    || completed_argument_bytes != self.argument_bytes
                {
                    return Err(self.stream_error(
                        "provider tool finish is missing complete tool-call identity",
                    ));
                }
                if !self.reasoning_buffer.is_empty() {
                    chunks.push(ProviderChunk::ContinuationState(
                        DeepSeekReasoningReplayPayload {
                            reasoning_content: self.reasoning_buffer.clone(),
                        }
                        .into_state(),
                    ));
                }
                self.tool_parts.clear();
                self.reasoning_buffer.clear();
                self.text_protocol_tail.clear();
            }
            if matches!(choice.finish_reason.as_deref(), Some("stop")) {
                if self.has_tool_calls() {
                    return Err(
                        self.stream_error("provider stopped before completing streamed tool calls")
                    );
                }
                self.tool_parts.clear();
                self.reasoning_buffer.clear();
                self.text_protocol_tail.clear();
            }
        }
        Ok(chunks)
    }

    pub(crate) fn diagnostic(&self) -> sigil_kernel::ProviderDiagnosticV1 {
        use sigil_kernel::ProviderStreamFinishV1;
        sigil_kernel::ProviderDiagnosticV1::ToolStreamFinished {
            finish: match self.finish_reason {
                Some("tool_calls") => ProviderStreamFinishV1::ToolCalls,
                Some("stop") => ProviderStreamFinishV1::Stop,
                Some("length") => ProviderStreamFinishV1::Length,
                Some("content_filter") => ProviderStreamFinishV1::ContentFilter,
                Some(_) => ProviderStreamFinishV1::Other,
                None => ProviderStreamFinishV1::Missing,
            },
            tool_call_count: self.tool_indices.len() as u64,
            argument_fragments: self.argument_fragments as u64,
            argument_bytes: self.argument_bytes as u64,
            completed_calls: self.completed_calls as u64,
        }
    }

    pub(crate) fn has_tool_calls(&self) -> bool {
        !self.tool_indices.is_empty()
    }

    pub(crate) fn finish(&self) -> Result<()> {
        self.trace_stream("stream_end");
        if self.has_tool_calls() && self.finish_reason != Some("tool_calls") {
            return Err(
                self.stream_error("provider stream ended before completing streamed tool calls")
            );
        }
        Ok(())
    }

    fn trace_stream(&self, stage: &'static str) {
        debug!(target: "sigil_provider_deepseek", stage, finish_reason = self.finish_reason.unwrap_or("missing"),
            tool_call_count = self.tool_indices.len(), argument_fragments = self.argument_fragments,
            argument_bytes = self.argument_bytes, completed_calls = self.completed_calls,
            "chat tool stream diagnostic");
    }

    fn stream_error(&self, reason: &'static str) -> anyhow::Error {
        self.trace_stream("protocol_error");
        sigil_kernel::SafePersistenceError::ToolCallStreamInvalid {
            reason: format!("{reason}; finish_reason={}; tool_calls={}; argument_fragments={}; argument_bytes={}; completed_calls={}",
                self.finish_reason.unwrap_or("missing"), self.tool_indices.len(), self.argument_fragments,
                self.argument_bytes, self.completed_calls),
        }.into()
    }

    fn map_tool_delta(
        &mut self,
        chunks: &mut Vec<ProviderChunk>,
        delta: DeepSeekToolCallDelta,
    ) -> Result<()> {
        if !self.tool_indices.contains(&delta.index)
            && self.tool_indices.len() >= sigil_kernel::MAX_PROVIDER_TURN_TOOL_CALLS
        {
            return Err(self.stream_error("provider exceeded the streamed tool-call limit"));
        }
        self.tool_indices.insert(delta.index);
        let (name, arguments) = delta
            .function
            .map(|function| (function.name, function.arguments))
            .unwrap_or_default();
        if let Some(arguments) = arguments.as_ref() {
            self.argument_fragments = self.argument_fragments.saturating_add(1);
            self.argument_bytes = self.argument_bytes.saturating_add(arguments.len());
        }
        self.tool_parts
            .append_delta(chunks, delta.index, delta.id, name, arguments);
        if let Some(ProviderChunk::ToolCallStreamError(error)) = chunks.last() {
            self.trace_stream("protocol_error");
            return Err(error.clone().into());
        }
        Ok(())
    }

    fn reject_unstructured_native_tool_protocol(&mut self, text: &str) -> Result<()> {
        let mut observed = self.text_protocol_tail.clone();
        observed.push_str(text);
        if observed.contains(NATIVE_TOOL_PROTOCOL_SENTINEL) {
            return Err(ProviderProtocolViolation::UnstructuredToolInvocation.into());
        }

        let retained_chars = NATIVE_TOOL_PROTOCOL_SENTINEL
            .chars()
            .count()
            .saturating_sub(1);
        self.text_protocol_tail = observed
            .chars()
            .rev()
            .take(retained_chars)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/mapper_tests.rs"]
mod tests;
