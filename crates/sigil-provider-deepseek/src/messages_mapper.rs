use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use sigil_kernel::{
    CacheTokenCountV1, CacheUsageV1, HostedCitationCandidate, HostedEvidence,
    HostedSourceCandidate, HostedToolKind, ProviderChunk, SecretString, ToolCallCompletionIdPolicy,
    ToolCallStreamAccumulator, UsageStats, WebSearchFailureClass,
};

use crate::messages_continuation::DeepSeekHostedStreamContext;
use crate::messages_models::{
    DeepSeekCitation, DeepSeekContentBlock, DeepSeekContentBlockDelta, DeepSeekMessagesEnvelope,
    DeepSeekUsage, DeepSeekWebSearchToolResultContent,
};

const MAX_SERVER_TOOL_INPUT_BYTES: usize = 64 * 1024;

struct ServerToolPart {
    invocation_id: String,
    partial_input: String,
}

struct SourceBinding {
    provider_source_id: String,
    invocation_id: String,
}

/// Maps DeepSeek Anthropic-compatible Messages SSE envelopes into kernel
/// provider chunks, including the hosted web-search lifecycle
/// (started -> query observed -> sources/citations -> usage -> continuation).
pub struct MessagesStreamMapper {
    tool_parts: ToolCallStreamAccumulator,
    hosted: Option<DeepSeekHostedStreamContext>,
    server_parts: BTreeMap<usize, ServerToolPart>,
    client_tool_inputs: BTreeMap<usize, String>,
    exact_blocks: BTreeMap<usize, Value>,
    source_by_url: BTreeMap<String, Vec<SourceBinding>>,
    emitted_sources: BTreeSet<String>,
    started_invocations: BTreeSet<String>,
    last_text_delta_by_block: BTreeMap<usize, (usize, usize)>,
    assistant_text: String,
    hosted_started: bool,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
    web_search_requests: u32,
    stop_reason: Option<String>,
    usage_emitted: bool,
}

impl MessagesStreamMapper {
    pub fn new(hosted: Option<DeepSeekHostedStreamContext>) -> Self {
        Self {
            tool_parts: ToolCallStreamAccumulator::new(),
            hosted,
            server_parts: BTreeMap::new(),
            client_tool_inputs: BTreeMap::new(),
            exact_blocks: BTreeMap::new(),
            source_by_url: BTreeMap::new(),
            emitted_sources: BTreeSet::new(),
            started_invocations: BTreeSet::new(),
            last_text_delta_by_block: BTreeMap::new(),
            assistant_text: String::new(),
            hosted_started: false,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            web_search_requests: 0,
            stop_reason: None,
            usage_emitted: false,
        }
    }

    pub fn map_envelope(
        &mut self,
        envelope: DeepSeekMessagesEnvelope,
    ) -> Result<Vec<ProviderChunk>> {
        let mut chunks = Vec::new();
        match envelope {
            DeepSeekMessagesEnvelope::MessageStart { message } => {
                if let Some(usage) = message.usage {
                    self.record_usage(&usage);
                }
            }
            DeepSeekMessagesEnvelope::ContentBlockStart {
                index,
                content_block,
            } => self.map_content_block_start(&mut chunks, index, content_block)?,
            DeepSeekMessagesEnvelope::ContentBlockDelta { index, delta } => {
                self.map_content_block_delta(&mut chunks, index, delta)?;
            }
            DeepSeekMessagesEnvelope::ContentBlockStop { index } => {
                self.finish_content_block(&mut chunks, index)?;
            }
            DeepSeekMessagesEnvelope::MessageDelta { delta, usage } => {
                if let Some(stop_reason) = delta.stop_reason {
                    self.stop_reason = Some(stop_reason);
                }
                if let Some(usage) = usage {
                    self.record_usage(&usage);
                }
            }
            DeepSeekMessagesEnvelope::MessageStop => {
                self.complete_open_tool_calls(&mut chunks);
                self.emit_usage(&mut chunks);
                self.emit_hosted_continuation(&mut chunks)?;
                chunks.push(ProviderChunk::Done);
            }
            DeepSeekMessagesEnvelope::Ping => {}
            DeepSeekMessagesEnvelope::Error { error } => {
                let message = if error.message.is_empty() {
                    error.r#type
                } else {
                    format!("{}: {}", error.r#type, error.message)
                };
                return Err(anyhow!("DeepSeek messages stream error: {message}"));
            }
        }
        Ok(chunks)
    }

    pub fn finish(&mut self) -> Result<Vec<ProviderChunk>> {
        if self.hosted.is_some() && self.hosted_started {
            return Err(anyhow!(
                "DeepSeek hosted stream ended before message_stop; continuation is unsafe"
            ));
        }
        let mut chunks = Vec::new();
        self.complete_open_tool_calls(&mut chunks);
        self.emit_usage(&mut chunks);
        Ok(chunks)
    }

    fn map_content_block_start(
        &mut self,
        chunks: &mut Vec<ProviderChunk>,
        index: usize,
        content_block: DeepSeekContentBlock,
    ) -> Result<()> {
        match content_block {
            DeepSeekContentBlock::Text { text } => {
                self.exact_blocks
                    .insert(index, json!({ "type": "text", "text": text }));
                if !text.is_empty() {
                    self.assistant_text.push_str(&text);
                    chunks.push(ProviderChunk::TextDelta(text));
                }
            }
            DeepSeekContentBlock::ToolUse { id, name, input } => {
                self.exact_blocks.insert(
                    index,
                    json!({ "type": "tool_use", "id": id, "name": name, "input": input }),
                );
                let arguments = if input.is_object()
                    && input.as_object().is_some_and(|object| !object.is_empty())
                {
                    Some(serde_json::to_string(&input)?)
                } else {
                    None
                };
                self.tool_parts
                    .append_delta(chunks, index, Some(id), Some(name), arguments);
                self.client_tool_inputs.insert(
                    index,
                    if input.as_object().is_some_and(|object| !object.is_empty()) {
                        serde_json::to_string(&input)?
                    } else {
                        String::new()
                    },
                );
            }
            DeepSeekContentBlock::ServerToolUse { id, name, input } => {
                if name != "web_search" {
                    return Err(anyhow!("unsupported DeepSeek server tool {name}"));
                }
                validate_invocation_id(&id)?;
                let Some(hosted) = self.hosted.as_ref() else {
                    return Err(anyhow!(
                        "DeepSeek emitted server web search for a non-hosted request"
                    ));
                };
                self.exact_blocks.insert(
                    index,
                    json!({ "type": "server_tool_use", "id": id.clone(), "name": name, "input": input }),
                );
                self.server_parts.insert(
                    index,
                    ServerToolPart {
                        invocation_id: id.clone(),
                        partial_input: if input.as_object().is_some_and(|value| !value.is_empty()) {
                            serde_json::to_string(&input)?
                        } else {
                            String::new()
                        },
                    },
                );
                self.hosted_started = true;
                self.started_invocations.insert(id.clone());
                chunks.push(ProviderChunk::HostedToolStarted {
                    authorization_id: hosted.authorization_id.clone(),
                    invocation_id: id,
                    kind: HostedToolKind::WebSearch,
                });
            }
            DeepSeekContentBlock::WebSearchToolResult {
                tool_use_id,
                content,
            } => self.map_web_search_result(chunks, index, tool_use_id, content)?,
            DeepSeekContentBlock::Thinking {
                thinking,
                signature,
            } => {
                self.exact_blocks.insert(
                    index,
                    json!({ "type": "thinking", "thinking": thinking, "signature": signature }),
                );
                if !thinking.is_empty() {
                    chunks.push(ProviderChunk::ReasoningDelta(thinking));
                }
            }
            DeepSeekContentBlock::Other => {
                if self.hosted.is_some() {
                    return Err(anyhow!(
                        "unsupported DeepSeek content block in hosted continuation"
                    ));
                }
            }
        }
        Ok(())
    }

    fn map_content_block_delta(
        &mut self,
        chunks: &mut Vec<ProviderChunk>,
        index: usize,
        delta: DeepSeekContentBlockDelta,
    ) -> Result<()> {
        match delta {
            DeepSeekContentBlockDelta::TextDelta { text } => {
                append_exact_string_if_required(
                    &mut self.exact_blocks,
                    index,
                    "text",
                    &text,
                    self.hosted.is_some(),
                )?;
                let start_byte = self.assistant_text.len();
                self.assistant_text.push_str(&text);
                self.last_text_delta_by_block
                    .insert(index, (start_byte, self.assistant_text.len()));
                chunks.push(ProviderChunk::TextDelta(text));
            }
            DeepSeekContentBlockDelta::InputJsonDelta { partial_json } => {
                if let Some(part) = self.server_parts.get_mut(&index) {
                    if part.partial_input.len().saturating_add(partial_json.len())
                        > MAX_SERVER_TOOL_INPUT_BYTES
                    {
                        return Err(anyhow!("DeepSeek server-tool input exceeds the hard limit"));
                    }
                    part.partial_input.push_str(&partial_json);
                } else if !partial_json.is_empty() {
                    let input = self.client_tool_inputs.entry(index).or_default();
                    if input.len().saturating_add(partial_json.len())
                        > sigil_kernel::MAX_STREAMED_TOOL_ARGS_BYTES
                    {
                        return Err(anyhow!("DeepSeek client-tool input exceeds the hard limit"));
                    }
                    input.push_str(&partial_json);
                    self.tool_parts
                        .append_delta(chunks, index, None, None, Some(partial_json));
                }
            }
            DeepSeekContentBlockDelta::ThinkingDelta { thinking } => {
                append_exact_string_if_required(
                    &mut self.exact_blocks,
                    index,
                    "thinking",
                    &thinking,
                    self.hosted.is_some(),
                )?;
                if !thinking.is_empty() {
                    chunks.push(ProviderChunk::ReasoningDelta(thinking));
                }
            }
            DeepSeekContentBlockDelta::SignatureDelta { signature } => {
                append_exact_string_if_required(
                    &mut self.exact_blocks,
                    index,
                    "signature",
                    &signature,
                    self.hosted.is_some(),
                )?;
            }
            DeepSeekContentBlockDelta::CitationsDelta { citation } => {
                self.map_citation(chunks, index, citation)?;
            }
            DeepSeekContentBlockDelta::Other => {}
        }
        Ok(())
    }

    fn finish_content_block(
        &mut self,
        chunks: &mut Vec<ProviderChunk>,
        index: usize,
    ) -> Result<()> {
        if let Some(part) = self.server_parts.remove(&index) {
            let input = if part.partial_input.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str::<Value>(&part.partial_input)
                    .map_err(|_| anyhow!("invalid DeepSeek server-tool input JSON"))?
            };
            if !input.is_object() {
                return Err(anyhow!("DeepSeek server-tool input must be an object"));
            }
            if let Some(block) = self.exact_blocks.get_mut(&index) {
                block["input"] = input.clone();
            }
            if let Some(query) = input.get("query").and_then(Value::as_str) {
                let authorization_id = self
                    .hosted
                    .as_ref()
                    .ok_or_else(|| anyhow!("hosted context disappeared"))?
                    .authorization_id
                    .clone();
                chunks.push(ProviderChunk::HostedEvidence {
                    authorization_id,
                    invocation_id: part.invocation_id,
                    kind: HostedToolKind::WebSearch,
                    evidence: HostedEvidence::QueryObserved(SecretString::new(query)),
                });
            }
        }
        if self
            .exact_blocks
            .get(&index)
            .and_then(|block| block.get("type"))
            .and_then(Value::as_str)
            == Some("tool_use")
        {
            let input = self.client_tool_inputs.remove(&index).unwrap_or_default();
            let input = if input.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str::<Value>(&input)
                    .map_err(|_| anyhow!("invalid DeepSeek client-tool input JSON"))?
            };
            if let Some(block) = self.exact_blocks.get_mut(&index) {
                block["input"] = input;
            }
            self.complete_open_tool_calls(chunks);
        }
        Ok(())
    }

    fn map_web_search_result(
        &mut self,
        chunks: &mut Vec<ProviderChunk>,
        index: usize,
        invocation_id: String,
        content: DeepSeekWebSearchToolResultContent,
    ) -> Result<()> {
        let authorization_id = self.authorization_for_invocation(&invocation_id)?;
        self.exact_blocks.insert(
            index,
            json!({
                "type": "web_search_tool_result",
                "tool_use_id": invocation_id,
                "content": serde_json::to_value(&content)?,
            }),
        );
        match content {
            DeepSeekWebSearchToolResultContent::Results(results) => {
                for (result_index, result) in results.iter().enumerate() {
                    let source = HostedSourceCandidate::new(
                        result.url.clone(),
                        result.url.clone(),
                        Some(result.title.clone()),
                    );
                    self.source_by_url
                        .entry(result.url.clone())
                        .or_default()
                        .push(SourceBinding {
                            provider_source_id: result.url.clone(),
                            invocation_id: invocation_id.clone(),
                        });
                    if self.emitted_sources.insert(result.url.clone()) {
                        chunks.push(ProviderChunk::HostedEvidence {
                            authorization_id: authorization_id.clone(),
                            invocation_id: invocation_id.clone(),
                            kind: HostedToolKind::WebSearch,
                            evidence: HostedEvidence::Source(source.with_rank(result_index)),
                        });
                    }
                }
            }
            DeepSeekWebSearchToolResultContent::Error(error) => {
                chunks.push(ProviderChunk::HostedToolFailed {
                    authorization_id,
                    invocation_id,
                    kind: HostedToolKind::WebSearch,
                    failure_class: map_search_error(&error.error_code),
                });
            }
        }
        Ok(())
    }

    fn map_citation(
        &mut self,
        chunks: &mut Vec<ProviderChunk>,
        text_block_index: usize,
        citation: DeepSeekCitation,
    ) -> Result<()> {
        let exact_citation = serde_json::to_value(&citation)?;
        let DeepSeekCitation::WebSearchResultLocation { url, .. } = citation else {
            return Ok(());
        };
        let hosted = self
            .hosted
            .as_ref()
            .ok_or_else(|| anyhow!("DeepSeek citation arrived for a non-hosted request"))?;
        if let Some(block) = self.exact_blocks.get_mut(&text_block_index)
            && let Some(object) = block.as_object_mut()
        {
            object
                .entry("citations")
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or_else(|| anyhow!("DeepSeek text citations field is not an array"))?
                .push(exact_citation);
        }
        let Some(bindings) = self.source_by_url.get(&url) else {
            return Ok(());
        };
        let [binding] = bindings.as_slice() else {
            return Ok(());
        };
        let Some((start_byte, end_byte)) = self
            .last_text_delta_by_block
            .get(&text_block_index)
            .copied()
        else {
            return Ok(());
        };
        if start_byte >= end_byte || self.assistant_text.get(start_byte..end_byte).is_none() {
            return Ok(());
        }
        chunks.push(ProviderChunk::HostedEvidence {
            authorization_id: hosted.authorization_id.clone(),
            invocation_id: binding.invocation_id.clone(),
            kind: HostedToolKind::WebSearch,
            evidence: HostedEvidence::Citation(HostedCitationCandidate::new(
                binding.provider_source_id.clone(),
                start_byte,
                end_byte,
            )),
        });
        Ok(())
    }

    fn authorization_for_invocation(&self, invocation_id: &str) -> Result<String> {
        let hosted = self
            .hosted
            .as_ref()
            .ok_or_else(|| anyhow!("DeepSeek search result arrived for a non-hosted request"))?;
        if self
            .server_parts
            .values()
            .any(|part| part.invocation_id == invocation_id)
            || self.exact_blocks.values().any(|block| {
                block.get("type").and_then(Value::as_str) == Some("server_tool_use")
                    && block.get("id").and_then(Value::as_str) == Some(invocation_id)
            })
        {
            return Ok(hosted.authorization_id.clone());
        }
        hosted
            .prior_invocations
            .get(invocation_id)
            .cloned()
            .ok_or_else(|| anyhow!("DeepSeek search result has no matching server-tool use"))
    }

    fn record_usage(&mut self, usage: &DeepSeekUsage) {
        self.input_tokens = self.input_tokens.max(usage.input_tokens);
        self.output_tokens = self.output_tokens.max(usage.output_tokens);
        self.cache_read_input_tokens = self
            .cache_read_input_tokens
            .max(usage.cache_read_input_tokens);
        self.cache_creation_input_tokens = self
            .cache_creation_input_tokens
            .max(usage.cache_creation_input_tokens);
        self.web_search_requests = self.web_search_requests.max(
            usage
                .server_tool_use
                .as_ref()
                .map_or(0, |server| server.web_search_requests),
        );
    }

    fn complete_open_tool_calls(&mut self, chunks: &mut Vec<ProviderChunk>) {
        self.tool_parts
            .complete_open_calls(chunks, ToolCallCompletionIdPolicy::RequireProviderId);
    }

    fn emit_usage(&mut self, chunks: &mut Vec<ProviderChunk>) {
        if self.web_search_requests > 0
            && let Some(hosted) = self.hosted.as_ref()
        {
            chunks.push(ProviderChunk::HostedRequestUsage {
                authorization_id: hosted.authorization_id.clone(),
                kind: HostedToolKind::WebSearch,
                observed_uses: self.web_search_requests,
            });
        }
        if self.usage_emitted
            || (self.input_tokens == 0
                && self.output_tokens == 0
                && self.cache_creation_input_tokens == 0
                && self.cache_read_input_tokens == 0)
        {
            return;
        }
        let cache_hit_tokens = self.cache_read_input_tokens;
        let cache_write_tokens = self.cache_creation_input_tokens;
        let total_input_tokens = self
            .input_tokens
            .saturating_add(cache_hit_tokens)
            .saturating_add(cache_write_tokens);
        chunks.push(ProviderChunk::Usage(UsageStats {
            prompt_tokens: total_input_tokens,
            completion_tokens: self.output_tokens,
            cache_hit_tokens,
            cache_miss_tokens: self.input_tokens.saturating_add(cache_write_tokens),
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
            cache_usage: Some(CacheUsageV1 {
                schema_version: CacheUsageV1::SCHEMA_VERSION,
                read: Some(CacheTokenCountV1::provider_reported(cache_hit_tokens)),
                write: Some(CacheTokenCountV1::provider_reported(cache_write_tokens)),
                uncached: Some(CacheTokenCountV1::provider_reported(self.input_tokens)),
                local_layout_mutation: None,
                provider_miss_without_local_mutation: false,
            }),
            pricing_snapshot: None,
        }));
        self.usage_emitted = true;
    }

    fn emit_hosted_continuation(&mut self, chunks: &mut Vec<ProviderChunk>) -> Result<()> {
        let Some(hosted) = self.hosted.as_ref() else {
            return Ok(());
        };
        if !self.hosted_started && hosted.prior_invocations.is_empty() {
            return Ok(());
        }
        let blocks = std::mem::take(&mut self.exact_blocks)
            .into_values()
            .collect::<Vec<_>>();
        let continuation_reason = match self.stop_reason.as_deref() {
            Some("pause_turn") => "pause_turn",
            Some("tool_use") => "mixed_tool_use",
            _ => "hosted_turn",
        };
        let state = hosted
            .continuation_store
            .retain_blocks(blocks, continuation_reason)?;
        chunks.push(ProviderChunk::ContinuationState(state));
        Ok(())
    }
}

fn append_exact_string_if_required(
    blocks: &mut BTreeMap<usize, Value>,
    index: usize,
    field: &str,
    delta: &str,
    required: bool,
) -> Result<()> {
    if !required {
        let has_field = blocks
            .get(&index)
            .and_then(Value::as_object)
            .is_some_and(|object| object.get(field).is_some_and(Value::is_string));
        if !has_field {
            return Ok(());
        }
    }
    let value = blocks
        .get_mut(&index)
        .and_then(Value::as_object_mut)
        .and_then(|object| object.get_mut(field))
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("DeepSeek content delta has no matching block"))?;
    blocks
        .get_mut(&index)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("DeepSeek content block is not an object"))?
        .insert(field.to_owned(), Value::String(value + delta));
    Ok(())
}

fn validate_invocation_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 512
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte)))
    {
        return Err(anyhow!("invalid DeepSeek server-tool invocation id"));
    }
    Ok(())
}

fn map_search_error(code: &str) -> WebSearchFailureClass {
    match code {
        "too_many_requests" => WebSearchFailureClass::RateLimited,
        "max_uses_exceeded" => WebSearchFailureClass::BudgetExhausted,
        "unavailable" => WebSearchFailureClass::ServiceUnavailable,
        "invalid_input" | "query_too_long" | "request_too_large" => {
            WebSearchFailureClass::ProtocolError
        }
        _ => WebSearchFailureClass::UnexpectedResponse,
    }
}

#[cfg(test)]
#[path = "tests/messages_mapper_tests.rs"]
mod tests;
