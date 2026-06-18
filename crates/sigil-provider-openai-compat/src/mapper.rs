use anyhow::Result;

use sigil_kernel::{
    ProviderChunk, ToolCallCompletionIdPolicy, ToolCallStreamAccumulator, UsageStats,
};

use crate::models::{OpenAiStreamEnvelope, OpenAiToolCallDelta};

pub struct StreamMapper {
    tool_parts: ToolCallStreamAccumulator,
}

impl StreamMapper {
    pub fn new() -> Self {
        Self {
            tool_parts: ToolCallStreamAccumulator::new(),
        }
    }
}

impl StreamMapper {
    pub fn map_envelope(&mut self, envelope: OpenAiStreamEnvelope) -> Result<Vec<ProviderChunk>> {
        let mut chunks = Vec::new();
        if let Some(usage) = envelope.usage {
            let cache_hit_tokens = usage
                .prompt_tokens_details
                .as_ref()
                .map(|details| details.cached_tokens)
                .unwrap_or_default();
            chunks.push(ProviderChunk::Usage(UsageStats {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                cache_hit_tokens,
                cache_miss_tokens: usage.prompt_tokens.saturating_sub(cache_hit_tokens),
                input_cost: 0.0,
                output_cost: 0.0,
                cache_savings: 0.0,
                system_fingerprint: envelope.system_fingerprint.clone(),
            }));
        }
        for choice in envelope.choices {
            if let Some(reasoning) = choice.delta.reasoning_content {
                chunks.push(ProviderChunk::ReasoningDelta(reasoning));
            }
            if let Some(content) = choice.delta.content {
                chunks.push(ProviderChunk::TextDelta(content));
            }
            if let Some(tool_calls) = choice.delta.tool_calls {
                for tool_call in tool_calls {
                    self.map_tool_delta(&mut chunks, tool_call);
                }
            }
            if matches!(choice.finish_reason.as_deref(), Some("tool_calls")) {
                self.complete_open_tool_calls(&mut chunks);
            }
            if matches!(choice.finish_reason.as_deref(), Some("stop")) {
                self.tool_parts.clear();
            }
        }
        Ok(chunks)
    }

    pub fn finish(&mut self) -> Vec<ProviderChunk> {
        let mut chunks = Vec::new();
        self.tool_parts
            .complete_open_calls(&mut chunks, ToolCallCompletionIdPolicy::SynthesizeFromIndex);
        chunks
    }

    fn map_tool_delta(&mut self, chunks: &mut Vec<ProviderChunk>, delta: OpenAiToolCallDelta) {
        let (name, arguments) = delta
            .function
            .map(|function| (function.name, function.arguments))
            .unwrap_or_default();
        self.tool_parts
            .append_delta(chunks, delta.index, delta.id, name, arguments);
    }

    fn complete_open_tool_calls(&mut self, chunks: &mut Vec<ProviderChunk>) {
        self.tool_parts
            .complete_open_calls(chunks, ToolCallCompletionIdPolicy::SynthesizeFromIndex);
    }
}

#[cfg(test)]
#[path = "tests/mapper_tests.rs"]
mod tests;
