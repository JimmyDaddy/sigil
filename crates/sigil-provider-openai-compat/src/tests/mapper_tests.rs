use anyhow::Result;
use sigil_kernel::ProviderChunk;

use crate::models::OpenAiStreamEnvelope;

use super::StreamMapper;

#[test]
fn map_envelope_emits_usage_text_and_tool_call_chunks() -> Result<()> {
    let envelope: OpenAiStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{
            "delta": {
                "content": "answer",
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
            "prompt_tokens_details": {
                "cached_tokens": 3
            }
        },
        "system_fingerprint": "fp-1"
    }))?;

    let mut mapper = StreamMapper::new();
    let chunks = mapper.map_envelope(envelope)?;

    assert!(matches!(
        chunks[0],
        ProviderChunk::Usage(ref usage)
        if usage.prompt_tokens == 10
            && usage.completion_tokens == 5
            && usage.cache_hit_tokens == 3
            && usage.cache_miss_tokens == 7
            && usage.system_fingerprint.as_deref() == Some("fp-1")
    ));
    assert!(matches!(chunks[1], ProviderChunk::TextDelta(ref text) if text == "answer"));
    assert!(matches!(
        chunks[2],
        ProviderChunk::ToolCallStart { ref id, ref name }
        if id == "call-1" && name == "read_file"
    ));
    assert!(matches!(
        chunks[3],
        ProviderChunk::ToolCallArgsDelta { ref id, ref delta }
        if id == "call-1" && delta == "{\"path\":"
    ));
    assert!(matches!(
        chunks[4],
        ProviderChunk::ToolCallArgsDelta { ref id, ref delta }
        if id == "call-1" && delta == "\"src/lib.rs\"}"
    ));
    assert!(matches!(
        chunks[5],
        ProviderChunk::ToolCallComplete(ref call)
        if call.id == "call-1"
            && call.name == "read_file"
            && call.args_json == "{\"path\":\"src/lib.rs\"}"
    ));
    Ok(())
}

#[test]
fn map_envelope_emits_reasoning_content_before_text() -> Result<()> {
    let envelope: OpenAiStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{
            "delta": {
                "reasoning_content": "think",
                "content": "answer"
            }
        }]
    }))?;

    let mut mapper = StreamMapper::new();
    let chunks = mapper.map_envelope(envelope)?;

    assert!(matches!(
        chunks.as_slice(),
        [
            ProviderChunk::ReasoningDelta(reasoning),
            ProviderChunk::TextDelta(text)
        ] if reasoning == "think" && text == "answer"
    ));
    Ok(())
}

#[test]
fn map_envelope_uses_synthetic_tool_id_and_finish_completes_late_name() -> Result<()> {
    let args: OpenAiStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 2,
                    "function": {
                        "arguments": "{\"value\":1}"
                    }
                }]
            }
        }]
    }))?;
    let name: OpenAiStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 2,
                    "function": {
                        "name": "echo"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    }))?;

    let mut mapper = StreamMapper::new();
    let first = mapper.map_envelope(args)?;
    let second = mapper.map_envelope(name)?;

    assert!(matches!(
        first.as_slice(),
        [ProviderChunk::ToolCallArgsDelta { id, delta }]
        if id == "call-2" && delta == "{\"value\":1}"
    ));
    assert!(matches!(
        second.as_slice(),
        [
            ProviderChunk::ToolCallStart { id, name },
            ProviderChunk::ToolCallComplete(call)
        ] if id == "call-2"
            && name == "echo"
            && call.id == "call-2"
            && call.args_json == "{\"value\":1}"
    ));
    Ok(())
}

#[test]
fn finish_completes_open_tool_calls_once_and_stop_clears_state() -> Result<()> {
    let tool: OpenAiStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call-1",
                    "function": {
                        "name": "echo",
                        "arguments": "{}"
                    }
                }]
            }
        }]
    }))?;
    let stop: OpenAiStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }))?;

    let mut mapper = StreamMapper::new();
    let chunks = mapper.map_envelope(tool)?;
    let finished = mapper.finish();
    let finished_again = mapper.finish();
    let stopped = mapper.map_envelope(stop)?;

    assert!(matches!(chunks[0], ProviderChunk::ToolCallStart { .. }));
    assert!(matches!(chunks[1], ProviderChunk::ToolCallArgsDelta { .. }));
    assert!(matches!(
        finished.as_slice(),
        [ProviderChunk::ToolCallComplete(call)] if call.name == "echo"
    ));
    assert!(finished_again.is_empty());
    assert!(stopped.is_empty());
    Ok(())
}

#[test]
fn finish_ignores_argument_only_tool_call_without_function_name() -> Result<()> {
    let envelope: OpenAiStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 3,
                    "function": {
                        "arguments": "{\"value\":1}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    }))?;

    let mut mapper = StreamMapper::new();
    let chunks = mapper.map_envelope(envelope)?;
    let finished = mapper.finish();

    assert!(matches!(
        chunks.as_slice(),
        [ProviderChunk::ToolCallArgsDelta { id, delta }]
        if id == "call-3" && delta == "{\"value\":1}"
    ));
    assert!(finished.is_empty());
    Ok(())
}
