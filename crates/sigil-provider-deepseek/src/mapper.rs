use anyhow::Result;

use sigil_kernel::{
    CacheTokenCountV1, CacheUsageV1, ProviderChunk, ProviderProtocolViolation,
    ToolCallCompletionIdPolicy, ToolCallStreamAccumulator, UsageStats,
};

use crate::{
    models::{DeepSeekStreamEnvelope, DeepSeekToolCallDelta},
    reasoning::DeepSeekReasoningReplayPayload,
};

pub struct StreamMapper {
    tool_parts: ToolCallStreamAccumulator,
    saw_tool_call: bool,
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
            saw_tool_call: false,
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
            if let Some(content) = choice.delta.content {
                self.reject_unstructured_native_tool_protocol(&content)?;
                chunks.push(ProviderChunk::TextDelta(content));
            }
            if let Some(reasoning_content) = choice.delta.reasoning_content {
                self.reasoning_buffer.push_str(&reasoning_content);
                chunks.push(ProviderChunk::ReasoningDelta(reasoning_content));
            }
            if let Some(tool_calls) = choice.delta.tool_calls {
                self.saw_tool_call = true;
                for tool_call in tool_calls {
                    self.map_tool_delta(&mut chunks, tool_call);
                }
            }
            if matches!(choice.finish_reason.as_deref(), Some("tool_calls")) {
                self.tool_parts.complete_open_calls(
                    &mut chunks,
                    ToolCallCompletionIdPolicy::RequireProviderId,
                );
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
                self.tool_parts.clear();
                self.reasoning_buffer.clear();
                self.text_protocol_tail.clear();
            }
        }
        Ok(chunks)
    }

    fn map_tool_delta(&mut self, chunks: &mut Vec<ProviderChunk>, delta: DeepSeekToolCallDelta) {
        let (name, arguments) = delta
            .function
            .map(|function| (function.name, function.arguments))
            .unwrap_or_default();
        self.tool_parts
            .append_delta(chunks, delta.index, delta.id, name, arguments);
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
