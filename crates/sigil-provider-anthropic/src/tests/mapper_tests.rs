use sigil_kernel::ProviderChunk;

use super::*;
use crate::models::AnthropicStreamEnvelope;

fn map_json(mapper: &mut StreamMapper, raw: &str) -> anyhow::Result<Vec<ProviderChunk>> {
    let envelope: AnthropicStreamEnvelope = serde_json::from_str(raw)?;
    mapper.map_envelope(envelope)
}

#[test]
fn stream_mapper_reconstructs_text_tool_use_usage_and_done() -> anyhow::Result<()> {
    let mut mapper = StreamMapper::new();
    let mut chunks = Vec::new();

    chunks.extend(map_json(
        &mut mapper,
        r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":1,"cache_read_input_tokens":4}}}"#,
    )?);
    chunks.extend(map_json(
        &mut mapper,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}"#,
    )?);
    chunks.extend(map_json(
        &mut mapper,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}"#,
    )?);
    chunks.extend(map_json(
        &mut mapper,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\""}}"#,
    )?);
    chunks.extend(map_json(
        &mut mapper,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":":\"src/lib.rs\"}"}}"#,
    )?);
    chunks.extend(map_json(
        &mut mapper,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":8}}"#,
    )?);
    chunks.extend(map_json(&mut mapper, r#"{"type":"message_stop"}"#)?);

    assert!(matches!(
        &chunks[0],
        ProviderChunk::TextDelta(delta) if delta == "hello"
    ));
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::ToolCallStart { id, name } if id == "toolu_1" && name == "read_file"
    )));
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::ToolCallComplete(call)
            if call.id == "toolu_1"
                && call.name == "read_file"
                && call.args_json == r#"{"path":"src/lib.rs"}"#
    )));
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::Usage(usage)
            if usage.prompt_tokens == 10
                && usage.completion_tokens == 8
                && usage.cache_hit_tokens == 4
                && usage.cache_miss_tokens == 6
    )));
    assert!(matches!(chunks.last(), Some(ProviderChunk::Done)));
    Ok(())
}

#[test]
fn stream_mapper_surfaces_anthropic_error_events() {
    let mut mapper = StreamMapper::new();

    let error = map_json(
        &mut mapper,
        r#"{"type":"error","error":{"type":"overloaded_error","message":"try later"}}"#,
    )
    .expect_err("error event should fail");

    assert!(error.to_string().contains("overloaded_error: try later"));
}

#[test]
fn finish_emits_pending_usage_without_done() -> anyhow::Result<()> {
    let mut mapper = StreamMapper::new();

    let chunks = map_json(
        &mut mapper,
        r#"{"type":"message_start","message":{"usage":{"input_tokens":3,"output_tokens":2}}}"#,
    )?;
    assert!(chunks.is_empty());

    let chunks = mapper.finish();
    assert!(matches!(
        chunks.first(),
        Some(ProviderChunk::Usage(usage)) if usage.prompt_tokens == 3 && usage.completion_tokens == 2
    ));
    Ok(())
}

#[test]
fn stream_mapper_covers_block_start_and_delta_edges() -> anyhow::Result<()> {
    let mut mapper = StreamMapper::new();
    let mut chunks = Vec::new();

    chunks.extend(map_json(
        &mut mapper,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"prefill"}}"#,
    )?);
    chunks.extend(map_json(
        &mut mapper,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_2","name":"grep","input":{"pattern":"fn","path":"src/lib.rs"}}}"#,
    )?);
    chunks.extend(map_json(
        &mut mapper,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":"checking"}}"#,
    )?);
    chunks.extend(map_json(
        &mut mapper,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"signature_delta","signature":"sig"}}"#,
    )?);
    chunks.extend(map_json(
        &mut mapper,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":""}}"#,
    )?);
    chunks.extend(map_json(
        &mut mapper,
        r#"{"type":"content_block_stop","index":1}"#,
    )?);
    chunks.extend(map_json(
        &mut mapper,
        r#"{"type":"content_block_start","index":2,"content_block":{"type":"unknown"}}"#,
    )?);
    chunks.extend(map_json(&mut mapper, r#"{"type":"ping"}"#)?);

    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::TextDelta(text) if text == "prefill"
    )));
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::ReasoningDelta(text) if text == "checking"
    )));
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::ToolCallArgsDelta { id, delta }
            if id == "toolu_2" && delta == r#"{"path":"src/lib.rs","pattern":"fn"}"#
    )));
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::ToolCallComplete(call)
            if call.id == "toolu_2"
                && call.name == "grep"
                && call.args_json == r#"{"path":"src/lib.rs","pattern":"fn"}"#
    )));
    Ok(())
}

#[test]
fn stream_mapper_surfaces_error_type_when_message_is_empty() {
    let mut mapper = StreamMapper::new();

    let error = map_json(
        &mut mapper,
        r#"{"type":"error","error":{"type":"api_error","message":""}}"#,
    )
    .expect_err("error event should fail");

    assert_eq!(error.to_string(), "Anthropic stream error: api_error");
}
