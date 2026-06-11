use anyhow::Result;
use sigil_kernel::ProviderChunk;

use crate::models::DeepSeekStreamEnvelope;

use super::StreamMapper;

#[test]
fn map_envelope_emits_usage_reasoning_tool_chunks_and_continuation_state() -> Result<()> {
    let envelope: DeepSeekStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{
            "delta": {
                "content": "answer",
                "reasoning_content": "think",
                "tool_calls": [
                    {
                        "index": 0,
                        "id": "call-1",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":"
                        }
                    },
                    {
                        "index": 0,
                        "function": {
                            "arguments": "\"src/lib.rs\"}"
                        }
                    }
                ]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "prompt_cache_hit_tokens": 3,
            "prompt_cache_miss_tokens": 7
        },
        "system_fingerprint": "fp-1"
    }))?;

    let mut mapper = StreamMapper::new("deepseek-v4-flash");
    let chunks = mapper.map_envelope(envelope)?;

    assert!(matches!(chunks[0], ProviderChunk::Usage(_)));
    assert!(matches!(chunks[1], ProviderChunk::TextDelta(ref text) if text == "answer"));
    assert!(matches!(chunks[2], ProviderChunk::ReasoningDelta(ref text) if text == "think"));
    assert!(matches!(
        chunks[3],
        ProviderChunk::ToolCallStart { ref id, ref name }
        if id == "call-1" && name == "read_file"
    ));
    assert!(matches!(
        chunks[4],
        ProviderChunk::ToolCallArgsDelta { ref id, ref delta }
        if id == "call-1" && delta == "{\"path\":"
    ));
    assert!(matches!(
        chunks[5],
        ProviderChunk::ToolCallArgsDelta { ref id, ref delta }
        if id == "call-1" && delta == "\"src/lib.rs\"}"
    ));
    assert!(matches!(
        chunks[6],
        ProviderChunk::ToolCallComplete(ref call)
        if call.id == "call-1"
            && call.name == "read_file"
            && call.args_json == "{\"path\":\"src/lib.rs\"}"
    ));
    assert!(matches!(
        chunks[7],
        ProviderChunk::ContinuationState(ref state)
        if state.state_kind == "deepseek.reasoning_replay"
            && state.opaque_blob["reasoning_content"] == "think"
    ));
    Ok(())
}

#[test]
fn map_envelope_uses_synthetic_tool_id_and_clears_state_on_stop() -> Result<()> {
    let start: DeepSeekStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 2,
                    "function": {
                        "arguments": "{\"value\":1}"
                    }
                }],
                "reasoning_content": "partial"
            }
        }]
    }))?;
    let stop: DeepSeekStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 2,
                    "function": {
                        "name": "echo"
                    }
                }]
            },
            "finish_reason": "stop"
        }]
    }))?;

    let mut mapper = StreamMapper::new("deepseek-v4-flash");
    let first = mapper.map_envelope(start)?;
    let second = mapper.map_envelope(stop)?;

    assert!(matches!(
        first.as_slice(),
        [
            ProviderChunk::ReasoningDelta(reasoning),
            ProviderChunk::ToolCallArgsDelta { id, delta }
        ] if reasoning == "partial" && id == "call-2" && delta == "{\"value\":1}"
    ));
    assert!(matches!(
        second.as_slice(),
        [ProviderChunk::ToolCallStart { id, name }] if id == "call-2" && name == "echo"
    ));
    Ok(())
}
