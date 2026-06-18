use anyhow::Result;

use sigil_kernel::{
    ProviderChunk, ToolCallCompletionIdPolicy, ToolCallStreamAccumulator, UsageStats,
};

use crate::{
    errors::AnthropicProviderError,
    models::{
        AnthropicContentBlock, AnthropicContentBlockDelta, AnthropicStreamEnvelope, AnthropicUsage,
    },
};

pub struct StreamMapper {
    tool_parts: ToolCallStreamAccumulator,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    usage_emitted: bool,
}

impl StreamMapper {
    pub fn new() -> Self {
        Self {
            tool_parts: ToolCallStreamAccumulator::new(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            usage_emitted: false,
        }
    }

    pub fn map_envelope(
        &mut self,
        envelope: AnthropicStreamEnvelope,
    ) -> Result<Vec<ProviderChunk>> {
        let mut chunks = Vec::new();
        match envelope {
            AnthropicStreamEnvelope::MessageStart { message } => {
                if let Some(usage) = message.usage {
                    self.record_usage(&usage);
                }
            }
            AnthropicStreamEnvelope::ContentBlockStart {
                index,
                content_block,
            } => self.map_content_block_start(&mut chunks, index, content_block)?,
            AnthropicStreamEnvelope::ContentBlockDelta { index, delta } => {
                self.map_content_block_delta(&mut chunks, index, delta);
            }
            AnthropicStreamEnvelope::ContentBlockStop { index } => {
                let _ = index;
                self.complete_open_tool_calls(&mut chunks);
            }
            AnthropicStreamEnvelope::MessageDelta { delta, usage } => {
                if let Some(usage) = usage {
                    self.record_usage(&usage);
                }
                if matches!(delta.stop_reason.as_deref(), Some("tool_use")) {
                    self.complete_open_tool_calls(&mut chunks);
                }
            }
            AnthropicStreamEnvelope::MessageStop => {
                self.complete_open_tool_calls(&mut chunks);
                self.emit_usage(&mut chunks);
                chunks.push(ProviderChunk::Done);
            }
            AnthropicStreamEnvelope::Ping => {}
            AnthropicStreamEnvelope::Error { error } => {
                let message = if error.message.is_empty() {
                    error.r#type
                } else {
                    format!("{}: {}", error.r#type, error.message)
                };
                return Err(AnthropicProviderError::Stream(message).into());
            }
        }
        Ok(chunks)
    }

    pub fn finish(&mut self) -> Vec<ProviderChunk> {
        let mut chunks = Vec::new();
        self.complete_open_tool_calls(&mut chunks);
        self.emit_usage(&mut chunks);
        chunks
    }

    fn map_content_block_start(
        &mut self,
        chunks: &mut Vec<ProviderChunk>,
        index: usize,
        content_block: AnthropicContentBlock,
    ) -> Result<()> {
        match content_block {
            AnthropicContentBlock::Text { text } => {
                if !text.is_empty() {
                    chunks.push(ProviderChunk::TextDelta(text));
                }
            }
            AnthropicContentBlock::ToolUse { id, name, input } => {
                let arguments = if input.is_object()
                    && input.as_object().is_some_and(|object| !object.is_empty())
                {
                    Some(serde_json::to_string(&input)?)
                } else {
                    None
                };
                self.tool_parts
                    .append_delta(chunks, index, Some(id), Some(name), arguments);
            }
            AnthropicContentBlock::Other => {}
        }
        Ok(())
    }

    fn map_content_block_delta(
        &mut self,
        chunks: &mut Vec<ProviderChunk>,
        index: usize,
        delta: AnthropicContentBlockDelta,
    ) {
        match delta {
            AnthropicContentBlockDelta::TextDelta { text } => {
                chunks.push(ProviderChunk::TextDelta(text));
            }
            AnthropicContentBlockDelta::InputJsonDelta { partial_json } => {
                if !partial_json.is_empty() {
                    self.tool_parts
                        .append_delta(chunks, index, None, None, Some(partial_json));
                }
            }
            AnthropicContentBlockDelta::ThinkingDelta { thinking } => {
                if !thinking.is_empty() {
                    chunks.push(ProviderChunk::ReasoningDelta(thinking));
                }
            }
            AnthropicContentBlockDelta::SignatureDelta { signature } => {
                let _ = signature;
            }
            AnthropicContentBlockDelta::Other => {}
        }
    }

    fn record_usage(&mut self, usage: &AnthropicUsage) {
        self.input_tokens = self.input_tokens.max(usage.input_tokens);
        self.output_tokens = self.output_tokens.max(usage.output_tokens);
        self.cache_read_input_tokens = self
            .cache_read_input_tokens
            .max(usage.cache_read_input_tokens);
        let _ = usage.cache_creation_input_tokens;
    }

    fn complete_open_tool_calls(&mut self, chunks: &mut Vec<ProviderChunk>) {
        self.tool_parts
            .complete_open_calls(chunks, ToolCallCompletionIdPolicy::RequireProviderId);
    }

    fn emit_usage(&mut self, chunks: &mut Vec<ProviderChunk>) {
        if self.usage_emitted
            || (self.input_tokens == 0
                && self.output_tokens == 0
                && self.cache_read_input_tokens == 0)
        {
            return;
        }
        let cache_hit_tokens = self.cache_read_input_tokens;
        chunks.push(ProviderChunk::Usage(UsageStats {
            prompt_tokens: self.input_tokens,
            completion_tokens: self.output_tokens,
            cache_hit_tokens,
            cache_miss_tokens: self.input_tokens.saturating_sub(cache_hit_tokens),
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        }));
        self.usage_emitted = true;
    }
}

#[cfg(test)]
#[path = "tests/mapper_tests.rs"]
mod tests;
