use std::collections::BTreeMap;

use anyhow::Result;

use sigil_kernel::{ProviderChunk, ToolCall, UsageStats};

use crate::models::{OpenAiStreamEnvelope, OpenAiToolCallDelta};

pub struct StreamMapper {
    tool_parts: BTreeMap<usize, ToolAccumulator>,
}

impl StreamMapper {
    pub fn new() -> Self {
        Self {
            tool_parts: BTreeMap::new(),
        }
    }
}

#[derive(Default)]
struct ToolAccumulator {
    id: Option<String>,
    name: Option<String>,
    args: String,
    started: bool,
    completed: bool,
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
        self.complete_open_tool_calls(&mut chunks);
        chunks
    }

    fn map_tool_delta(&mut self, chunks: &mut Vec<ProviderChunk>, delta: OpenAiToolCallDelta) {
        let accumulator = self.tool_parts.entry(delta.index).or_default();
        if let Some(id) = delta.id {
            accumulator.id = Some(id);
        }
        if let Some(function) = delta.function {
            if let Some(name) = function.name {
                accumulator.name = Some(name);
                emit_tool_start(chunks, delta.index, accumulator);
            }
            if let Some(arguments) = function.arguments {
                accumulator.args.push_str(&arguments);
                let id = tool_id(delta.index, accumulator);
                chunks.push(ProviderChunk::ToolCallArgsDelta {
                    id,
                    delta: arguments,
                });
            }
        }
    }

    fn complete_open_tool_calls(&mut self, chunks: &mut Vec<ProviderChunk>) {
        for (index, accumulator) in &mut self.tool_parts {
            emit_tool_start(chunks, *index, accumulator);
            if accumulator.completed {
                continue;
            }
            if let Some(name) = accumulator.name.clone() {
                chunks.push(ProviderChunk::ToolCallComplete(ToolCall {
                    id: tool_id(*index, accumulator),
                    name,
                    args_json: accumulator.args.clone(),
                }));
                accumulator.completed = true;
            }
        }
        self.tool_parts
            .retain(|_, accumulator| !accumulator.completed);
    }
}

fn emit_tool_start(
    chunks: &mut Vec<ProviderChunk>,
    index: usize,
    accumulator: &mut ToolAccumulator,
) {
    if accumulator.started {
        return;
    }
    let Some(name) = accumulator.name.clone() else {
        return;
    };
    chunks.push(ProviderChunk::ToolCallStart {
        id: tool_id(index, accumulator),
        name,
    });
    accumulator.started = true;
}

fn tool_id(index: usize, accumulator: &ToolAccumulator) -> String {
    accumulator
        .id
        .clone()
        .unwrap_or_else(|| format!("call-{index}"))
}

#[cfg(test)]
#[path = "tests/mapper_tests.rs"]
mod tests;
