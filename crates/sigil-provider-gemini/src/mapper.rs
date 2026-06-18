use anyhow::Result;

use sigil_kernel::{ProviderChunk, ToolCall, UsageStats};

use crate::{
    errors::GeminiProviderError,
    models::{GeminiFunctionCall, GeminiStreamEnvelope, GeminiUsageMetadata},
};

pub struct StreamMapper {
    latest_usage: Option<GeminiUsageMetadata>,
    next_synthetic_call_id: usize,
}

impl StreamMapper {
    pub fn new() -> Self {
        Self {
            latest_usage: None,
            next_synthetic_call_id: 0,
        }
    }

    pub fn map_envelope(&mut self, envelope: GeminiStreamEnvelope) -> Result<Vec<ProviderChunk>> {
        if let Some(prompt_feedback) = envelope.prompt_feedback
            && let Some(block_reason) = prompt_feedback.block_reason
        {
            return Err(GeminiProviderError::Blocked(block_reason).into());
        }

        if let Some(usage) = envelope.usage_metadata {
            self.latest_usage = Some(usage);
        }

        let mut chunks = Vec::new();
        for candidate in envelope.candidates {
            let _ = candidate.finish_reason;
            if let Some(content) = candidate.content {
                for part in content.parts {
                    if let Some(text) = part.text
                        && !text.is_empty()
                    {
                        chunks.push(ProviderChunk::TextDelta(text));
                    }
                    if let Some(function_call) = part.function_call {
                        self.map_function_call(&mut chunks, function_call)?;
                    }
                }
            }
        }
        Ok(chunks)
    }

    pub fn finish(&mut self) -> Vec<ProviderChunk> {
        let mut chunks = Vec::new();
        if let Some(usage) = self.latest_usage.take() {
            let cache_hit_tokens = usage.cached_content_token_count;
            chunks.push(ProviderChunk::Usage(UsageStats {
                prompt_tokens: usage.prompt_token_count,
                completion_tokens: usage.candidates_token_count,
                cache_hit_tokens,
                cache_miss_tokens: usage.prompt_token_count.saturating_sub(cache_hit_tokens),
                input_cost: 0.0,
                output_cost: 0.0,
                cache_savings: 0.0,
                system_fingerprint: None,
            }));
        }
        chunks
    }

    fn map_function_call(
        &mut self,
        chunks: &mut Vec<ProviderChunk>,
        function_call: GeminiFunctionCall,
    ) -> Result<()> {
        let id = function_call.id.unwrap_or_else(|| {
            let id = format!("call-{}", self.next_synthetic_call_id);
            self.next_synthetic_call_id += 1;
            id
        });
        let args_json = serde_json::to_string(&function_call.args)?;
        chunks.push(ProviderChunk::ToolCallStart {
            id: id.clone(),
            name: function_call.name.clone(),
        });
        chunks.push(ProviderChunk::ToolCallComplete(ToolCall {
            id,
            name: function_call.name,
            args_json,
        }));
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/mapper_tests.rs"]
mod tests;
