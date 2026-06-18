use sigil_kernel::ProviderChunk;

use super::*;
use crate::models::GeminiStreamEnvelope;

fn map_json(mapper: &mut StreamMapper, raw: &str) -> anyhow::Result<Vec<ProviderChunk>> {
    let envelope: GeminiStreamEnvelope = serde_json::from_str(raw)?;
    mapper.map_envelope(envelope)
}

#[test]
fn stream_mapper_maps_text_function_calls_and_final_usage() -> anyhow::Result<()> {
    let mut mapper = StreamMapper::new();
    let mut chunks = Vec::new();

    chunks.extend(map_json(
        &mut mapper,
        r#"{"candidates":[{"content":{"parts":[{"text":"hello"}]}}]}"#,
    )?);
    chunks.extend(map_json(
        &mut mapper,
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"id":"call-1","name":"read_file","args":{"path":"src/lib.rs"}}}]}}],"usageMetadata":{"promptTokenCount":12,"candidatesTokenCount":5,"cachedContentTokenCount":3}}"#,
    )?);
    chunks.extend(mapper.finish());

    assert!(matches!(
        &chunks[0],
        ProviderChunk::TextDelta(delta) if delta == "hello"
    ));
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::ToolCallStart { id, name } if id == "call-1" && name == "read_file"
    )));
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::ToolCallComplete(call)
            if call.id == "call-1"
                && call.name == "read_file"
                && call.args_json == r#"{"path":"src/lib.rs"}"#
    )));
    assert!(matches!(
        chunks.last(),
        Some(ProviderChunk::Usage(usage))
            if usage.prompt_tokens == 12
                && usage.completion_tokens == 5
                && usage.cache_hit_tokens == 3
                && usage.cache_miss_tokens == 9
    ));
    Ok(())
}

#[test]
fn stream_mapper_synthesizes_missing_function_call_ids() -> anyhow::Result<()> {
    let mut mapper = StreamMapper::new();

    let chunks = map_json(
        &mut mapper,
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"echo","args":{}}}]}}]}"#,
    )?;

    assert!(matches!(
        &chunks[0],
        ProviderChunk::ToolCallStart { id, name } if id == "call-0" && name == "echo"
    ));
    assert!(matches!(
        &chunks[1],
        ProviderChunk::ToolCallComplete(call) if call.id == "call-0" && call.name == "echo"
    ));
    Ok(())
}

#[test]
fn stream_mapper_surfaces_prompt_block_reason() {
    let mut mapper = StreamMapper::new();

    let error = map_json(
        &mut mapper,
        r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#,
    )
    .expect_err("blocked prompt should fail");

    assert!(error.to_string().contains("SAFETY"));
}
