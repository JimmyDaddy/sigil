use anyhow::Result;
use serde_json::json;

use sigil_kernel::{ProviderChunk, ProviderContinuationState, ToolCall, UsageStats};

use crate::{
    errors::GeminiProviderError,
    hosted_search::{GeminiGroundingAccumulator, GeminiHostedInvocation},
    models::{GeminiFunctionCall, GeminiSafetyRating, GeminiStreamEnvelope, GeminiUsageMetadata},
};

pub const GEMINI_THOUGHT_SIGNATURE_STATE_KIND: &str = "gemini.thought_signature";

pub struct StreamMapper {
    latest_usage: Option<GeminiUsageMetadata>,
    next_synthetic_call_id: usize,
    hosted_invocation: Option<GeminiHostedInvocation>,
    hosted_started: bool,
    grounding: GeminiGroundingAccumulator,
}

impl StreamMapper {
    pub fn new() -> Self {
        Self {
            latest_usage: None,
            next_synthetic_call_id: 0,
            hosted_invocation: None,
            hosted_started: false,
            grounding: GeminiGroundingAccumulator::new(),
        }
    }

    pub fn with_hosted(invocation: GeminiHostedInvocation) -> Self {
        Self {
            hosted_invocation: Some(invocation),
            ..Self::new()
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
            ensure_normal_finish(
                candidate.finish_reason.as_deref(),
                candidate.finish_message.as_deref(),
                &candidate.safety_ratings,
            )?;
            if let Some(content) = candidate.content {
                for (part_index, part) in content.parts.into_iter().enumerate() {
                    let thought_signature = part.thought_signature;
                    if let Some(text) = part.text
                        && !text.is_empty()
                    {
                        if self.hosted_invocation.is_some() {
                            self.grounding
                                .record_text(candidate.index, part_index, &text)?;
                        }
                        chunks.push(ProviderChunk::TextDelta(text));
                    }
                    if let Some(function_call) = part.function_call {
                        self.map_function_call(&mut chunks, function_call, thought_signature)?;
                    }
                }
            }
            if let Some(metadata) = candidate.grounding_metadata {
                let Some(invocation) = self.hosted_invocation.as_ref() else {
                    return Err(GeminiProviderError::UnexpectedGroundingMetadata.into());
                };
                if !self.hosted_started {
                    chunks.push(ProviderChunk::HostedToolStarted {
                        authorization_id: invocation.authorization_id.clone(),
                        invocation_id: invocation.invocation_id.clone(),
                        kind: sigil_kernel::HostedToolKind::WebSearch,
                    });
                    self.hosted_started = true;
                }
                chunks.extend(
                    self.grounding
                        .map_metadata(candidate.index, metadata)?
                        .into_iter()
                        .map(|evidence| ProviderChunk::HostedEvidence {
                            authorization_id: invocation.authorization_id.clone(),
                            invocation_id: invocation.invocation_id.clone(),
                            kind: sigil_kernel::HostedToolKind::WebSearch,
                            evidence,
                        }),
                );
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
        thought_signature: Option<String>,
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
            id: id.clone(),
            name: function_call.name,
            args_json,
        }));
        if let Some(thought_signature) = thought_signature
            && !thought_signature.trim().is_empty()
        {
            chunks.push(ProviderChunk::ContinuationState(
                ProviderContinuationState {
                    provider_name: "gemini".to_owned(),
                    state_kind: GEMINI_THOUGHT_SIGNATURE_STATE_KIND.to_owned(),
                    message_id: None,
                    opaque_blob: json!({
                        "tool_call_id": id,
                        "thought_signature": thought_signature,
                    }),
                },
            ));
        }
        Ok(())
    }
}

fn ensure_normal_finish(
    finish_reason: Option<&str>,
    finish_message: Option<&str>,
    safety_ratings: &[GeminiSafetyRating],
) -> Result<()> {
    let Some(reason) = finish_reason else {
        return Ok(());
    };
    if matches!(reason, "STOP" | "MAX_TOKENS" | "FINISH_REASON_UNSPECIFIED") {
        return Ok(());
    }
    let mut message = finish_message.unwrap_or_default().trim().to_owned();
    let safety_summary = safety_ratings
        .iter()
        .filter_map(|rating| {
            let category = rating.category.as_deref()?;
            let probability = rating.probability.as_deref().unwrap_or("unknown");
            let blocked = rating.blocked.unwrap_or(false);
            Some(format!("{category}:{probability}:blocked={blocked}"))
        })
        .collect::<Vec<_>>()
        .join(",");
    if !safety_summary.is_empty() {
        if !message.is_empty() {
            message.push_str("; ");
        }
        message.push_str("safety=");
        message.push_str(&safety_summary);
    }
    let formatted_message = if message.is_empty() {
        String::new()
    } else {
        format!(": {message}")
    };
    Err(GeminiProviderError::AbnormalFinish {
        reason: reason.to_owned(),
        message: formatted_message,
    }
    .into())
}

#[cfg(test)]
#[path = "tests/mapper_tests.rs"]
mod tests;
