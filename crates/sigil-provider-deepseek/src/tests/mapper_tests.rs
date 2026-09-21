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

    assert!(matches!(
        chunks[0],
        ProviderChunk::Usage(ref usage)
            if usage.pricing_snapshot.is_none()
                && usage.input_cost == 0.0
                && usage.output_cost == 0.0
    ));
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
fn map_envelope_rejects_stop_with_incomplete_tool_calls() -> Result<()> {
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
    let error = mapper
        .map_envelope(stop)
        .expect_err("stop cannot discard a pending tool call");

    assert!(matches!(
        first.as_slice(),
        [
            ProviderChunk::ReasoningDelta(reasoning),
            ProviderChunk::ToolCallArgsDelta { id, delta }
        ] if reasoning == "partial" && id == "call-2" && delta == "{\"value\":1}"
    ));
    assert!(error.to_string().contains("stopped before completing"));
    Ok(())
}

#[test]
fn map_envelope_keeps_stream_event_id_when_provider_id_arrives_late() -> Result<()> {
    let start: DeepSeekStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "function": {
                        "name": "echo",
                        "arguments": "{\"value\""
                    }
                }]
            }
        }]
    }))?;
    let finish: DeepSeekStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "provider-call-1",
                    "function": {
                        "arguments": ":1}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    }))?;

    let mut mapper = StreamMapper::new("deepseek-v4-flash");
    let first = mapper.map_envelope(start)?;
    let second = mapper.map_envelope(finish)?;

    assert!(matches!(
        first.as_slice(),
        [
            ProviderChunk::ToolCallStart { id, name },
            ProviderChunk::ToolCallArgsDelta { id: args_id, delta }
        ] if id == "call-0"
            && name == "echo"
            && args_id == "call-0"
            && delta == "{\"value\""
    ));
    assert!(matches!(
        second.as_slice(),
        [
            ProviderChunk::ToolCallArgsDelta { id, delta },
            ProviderChunk::ToolCallComplete(call)
        ] if id == "call-0"
            && delta == ":1}"
            && call.id == "call-0"
            && call.name == "echo"
            && call.args_json == "{\"value\":1}"
    ));
    Ok(())
}

#[test]
fn map_envelope_rejects_native_dsml_tool_protocol_split_across_text_deltas() -> Result<()> {
    let first: DeepSeekStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{"delta": {"content": "working <｜｜DSML｜｜tool_cal"}}]
    }))?;
    let second: DeepSeekStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{"delta": {"content": "ls>{\\\"name\\\":\\\"bash\\\"}"}}]
    }))?;

    let mut mapper = StreamMapper::new("deepseek-v4-flash");
    assert!(matches!(
        mapper.map_envelope(first)?.as_slice(),
        [ProviderChunk::TextDelta(text)] if text == "working <｜｜DSML｜｜tool_cal"
    ));
    let error = mapper
        .map_envelope(second)
        .expect_err("raw DSML must never become a final answer");
    assert_eq!(
        error.downcast_ref::<sigil_kernel::ProviderProtocolViolation>(),
        Some(&sigil_kernel::ProviderProtocolViolation::UnstructuredToolInvocation)
    );
    Ok(())
}

#[test]
fn map_envelope_preserves_malformed_arguments_for_tool_validation() -> Result<()> {
    let mut mapper = StreamMapper::new("deepseek-v4-flash");
    let envelope = serde_json::from_value(serde_json::json!({
        "choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call-1",
            "function": {"name": "task_completion_claim", "arguments": "{\"requirements\":[\"source\""}}]},
            "finish_reason": "tool_calls"}]
    }))?;
    let chunks = mapper.map_envelope(envelope)?;
    assert!(chunks.iter().any(
        |chunk| matches!(chunk, ProviderChunk::ToolCallComplete(call)
        if call.args_json == "{\"requirements\":[\"source\"")
    ));
    mapper.finish()?;
    assert_eq!(mapper.argument_fragments, 1);
    assert_eq!(mapper.completed_calls, 1);
    Ok(())
}

#[test]
fn map_envelope_rejects_truncation_and_missing_identity_without_exposing_arguments() -> Result<()> {
    for (finish_reason, include_id) in [("length", true), ("tool_calls", false)] {
        let mut mapper = StreamMapper::new("deepseek-v4-flash");
        let mut call = serde_json::json!({"index": 0,
            "function": {"name": "task_completion_claim", "arguments": "private-argument-marker"}});
        if include_id {
            call["id"] = serde_json::json!("call-1");
        }
        let envelope = serde_json::from_value(serde_json::json!({
            "choices": [{"delta": {"tool_calls": [call]}, "finish_reason": finish_reason}]
        }))?;
        let error = mapper
            .map_envelope(envelope)
            .expect_err("incomplete stream must fail");
        assert!(
            error
                .downcast_ref::<sigil_kernel::SafePersistenceError>()
                .is_some()
        );
        assert!(!error.to_string().contains("private-argument-marker"));
        assert!(error.to_string().contains("finish_reason="));
    }
    Ok(())
}

#[test]
fn map_envelope_classifies_text_only_length_finish_as_recoverable_truncation() -> Result<()> {
    let mut mapper = StreamMapper::new("deepseek-v4-flash");
    let envelope = serde_json::from_value(serde_json::json!({
        "choices": [{
            "delta": {"reasoning_content": "partial reasoning"},
            "finish_reason": "length"
        }]
    }))?;

    let error = mapper
        .map_envelope(envelope)
        .expect_err("length finish without tool calls is an incomplete generation");
    assert!(
        error
            .downcast_ref::<sigil_kernel::ProviderStreamEndedUnexpectedly>()
            .is_some()
    );
    assert!(matches!(
        mapper.diagnostic(),
        sigil_kernel::ProviderDiagnosticV1::ToolStreamFinished {
            finish: sigil_kernel::ProviderStreamFinishV1::Length,
            tool_call_count: 0,
            ..
        }
    ));
    Ok(())
}

#[test]
fn map_envelope_rejects_identity_drift_and_post_finish_calls() -> Result<()> {
    for second_id in ["different-call", "call-1"] {
        let mut mapper = StreamMapper::new("deepseek-v4-flash");
        let first = serde_json::from_value(serde_json::json!({
            "choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call-1",
                "function": {"name": "echo", "arguments": "{}"}}]},
                "finish_reason": if second_id == "call-1" { Some("tool_calls") } else { None }}]
        }))?;
        mapper.map_envelope(first)?;
        let second = serde_json::from_value(serde_json::json!({
            "choices": [{"delta": {"tool_calls": [{"index": 0, "id": second_id,
                "function": {"arguments": "private-argument-marker"}}]}}]
        }))?;
        let error = mapper
            .map_envelope(second)
            .expect_err("stream identity is immutable");
        assert!(!error.to_string().contains("private-argument-marker"));
    }
    Ok(())
}

#[test]
fn map_envelope_rejects_stream_end_before_tool_finish() -> Result<()> {
    let mut mapper = StreamMapper::new("deepseek-v4-flash");
    let envelope = serde_json::from_value(serde_json::json!({
        "choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call-1",
            "function": {"name": "echo", "arguments": "{\"value\":"}}]}}]
    }))?;
    mapper.map_envelope(envelope)?;
    let error = mapper
        .finish()
        .expect_err("EOF and DONE cannot complete partial arguments");
    assert!(error.to_string().contains("finish_reason=missing"));
    assert!(error.to_string().contains("argument_fragments=1"));
    Ok(())
}
