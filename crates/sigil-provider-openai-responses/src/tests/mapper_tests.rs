use anyhow::Result;
use sigil_kernel::ProviderChunk;

use crate::{
    OPENAI_RESPONSES_OUTPUT_ITEMS_STATE_KIND, mapper::StreamMapper,
    request::OPENAI_RESPONSES_PROVIDER_NAME,
};

#[test]
fn mapper_emits_streamed_text_tool_usage_and_exact_output_item_state() -> Result<()> {
    let output_items = vec![
        serde_json::json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{"type":"output_text", "text":"hello"}],
            "encrypted_content": "opaque-reasoning"
        }),
        serde_json::json!({
            "id": "fc_1",
            "type": "function_call",
            "call_id": "call_1",
            "name": "read_file",
            "arguments": "{\"path\":\"src/lib.rs\"}"
        }),
    ];
    let mut mapper = StreamMapper::new();
    let mut chunks = Vec::new();
    chunks.extend(mapper.map_event(
        "response.output_text.delta",
        serde_json::json!({"delta":"hello"}),
    )?);
    chunks.extend(mapper.map_event(
        "response.output_item.added",
        serde_json::json!({"item": output_items[1]}),
    )?);
    chunks.extend(mapper.map_event(
        "response.function_call_arguments.delta",
        serde_json::json!({"item_id":"fc_1", "delta":"{\"path\":"}),
    )?);
    chunks.extend(mapper.map_event(
        "response.function_call_arguments.delta",
        serde_json::json!({"item_id":"fc_1", "delta":"\"src/lib.rs\"}"}),
    )?);
    chunks.extend(mapper.map_event(
        "response.output_item.done",
        serde_json::json!({"item": output_items[1]}),
    )?);
    chunks.extend(mapper.map_event(
        "response.completed",
        serde_json::json!({
            "response": {
                "id": "resp_1",
                "status": "completed",
                "system_fingerprint": "fp_1",
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 3,
                    "input_tokens_details": {"cached_tokens": 4}
                },
                "output": output_items
            }
        }),
    )?);

    assert!(matches!(chunks[0], ProviderChunk::TextDelta(ref text) if text == "hello"));
    assert!(matches!(
        chunks[1],
        ProviderChunk::ToolCallStart { ref id, ref name } if id == "call_1" && name == "read_file"
    ));
    assert!(matches!(
        chunks[4],
        ProviderChunk::ToolCallComplete(ref call)
            if call.id == "call_1" && call.args_json == "{\"path\":\"src/lib.rs\"}"
    ));
    assert!(matches!(
        chunks[5],
        ProviderChunk::Usage(ref usage)
            if usage.prompt_tokens == 10 && usage.cache_hit_tokens == 4 && usage.system_fingerprint.as_deref() == Some("fp_1")
    ));
    assert!(matches!(
        chunks[6],
        ProviderChunk::ContinuationState(ref state)
            if state.provider_name == OPENAI_RESPONSES_PROVIDER_NAME
                && state.state_kind == OPENAI_RESPONSES_OUTPUT_ITEMS_STATE_KIND
                && state.opaque_blob["output_items"][0]["encrypted_content"] == "opaque-reasoning"
    ));
    assert!(matches!(chunks[7], ProviderChunk::Done));
    assert!(mapper.is_completed());
    Ok(())
}

#[test]
fn mapper_rejects_missing_function_item_and_missing_terminal_completion() -> Result<()> {
    let mut mapper = StreamMapper::new();
    let error = mapper
        .map_event(
            "response.function_call_arguments.delta",
            serde_json::json!({"item_id":"unknown", "delta":"{}"}),
        )
        .expect_err("unknown tool item must fail closed");

    assert!(error.to_string().contains("unknown function-call item"));
    assert!(!mapper.is_completed());
    Ok(())
}
