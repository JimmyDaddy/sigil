use anyhow::Result;

use sigil_kernel::{
    ProviderChunk, ToolCallCompletionIdPolicy, ToolCallStreamAccumulator, UsageStats,
};

use crate::{
    models::{DeepSeekStreamEnvelope, DeepSeekToolCallDelta},
    pricing::enrich_usage_costs,
    reasoning::DeepSeekReasoningReplayPayload,
};

pub struct StreamMapper {
    model: String,
    tool_parts: ToolCallStreamAccumulator,
    saw_tool_call: bool,
    reasoning_buffer: String,
}

impl StreamMapper {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            tool_parts: ToolCallStreamAccumulator::new(),
            saw_tool_call: false,
            reasoning_buffer: String::new(),
        }
    }
}

impl StreamMapper {
    pub fn map_envelope(&mut self, envelope: DeepSeekStreamEnvelope) -> Result<Vec<ProviderChunk>> {
        let mut chunks = Vec::new();
        if let Some(usage) = envelope.usage {
            chunks.push(ProviderChunk::Usage(enrich_usage_costs(
                &self.model,
                UsageStats {
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
                    cache_hit_tokens: usage.prompt_cache_hit_tokens.unwrap_or_default(),
                    cache_miss_tokens: usage.prompt_cache_miss_tokens.unwrap_or_default(),
                    input_cost: 0.0,
                    output_cost: 0.0,
                    cache_savings: 0.0,
                    system_fingerprint: envelope.system_fingerprint.clone(),
                },
            )));
        }
        for choice in envelope.choices {
            if let Some(content) = choice.delta.content {
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
            }
            if matches!(choice.finish_reason.as_deref(), Some("stop")) {
                self.tool_parts.clear();
                self.reasoning_buffer.clear();
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
}

#[cfg(test)]
#[path = "tests/mapper_tests.rs"]
mod tests;
