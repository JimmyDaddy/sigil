use super::*;
use crate::{mapper::StreamMapper, request::build_messages_request};
use anyhow::Result;
use serde_json::json;
use sigil_kernel::{CompletionRequest, ModelMessage, ProviderChunk, ToolCall};

fn request() -> CompletionRequest {
    let assistant = ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call-1".to_owned(),
            name: "read".to_owned(),
            args_json: "{}".to_owned(),
        }],
    );
    CompletionRequest {
        provider_name: "anthropic".to_owned(),
        model_name: "deepseek-flash".to_owned(),
        messages: vec![ModelMessage::user("Read."), assistant],
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(32768),
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    }
}

#[test]
fn thinking_replay_survives_durable_message_binding_and_rebuilds_exact_blocks() -> Result<()> {
    let mut mapper =
        StreamMapper::new(None).with_thinking_replay(Some("deepseek-flash".to_owned()));
    for event in [
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"first","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":" + second"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"signature"}}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
    ] {
        mapper.map_envelope(serde_json::from_value(event)?)?;
    }
    let chunks = mapper.map_envelope(serde_json::from_value(json!({"type":"message_stop"}))?)?;
    let mut state = chunks
        .into_iter()
        .find_map(|chunk| match chunk {
            ProviderChunk::ContinuationState(state) => Some(state),
            _ => None,
        })
        .expect("durable state");
    let mut request = request();
    state.message_id = Some(request.messages[1].id.clone());
    let entry = sigil_kernel::SessionLogEntry::Control(
        sigil_kernel::ControlEntry::ContinuationStateSaved(state),
    );
    let restored: sigil_kernel::SessionLogEntry =
        serde_json::from_slice(&serde_json::to_vec(&entry)?)?;
    let sigil_kernel::SessionLogEntry::Control(sigil_kernel::ControlEntry::ContinuationStateSaved(
        state,
    )) = restored
    else {
        anyhow::bail!("wrong durable type")
    };
    request.continuation_states.push(state);
    let mut body = build_messages_request(&request, 32768)?;
    apply(&mut body, &request)?;
    assert_eq!(
        body.messages[1]["content"][0],
        json!({"type":"thinking","thinking":"first + second","signature":"signature"})
    );
    assert_eq!(body.messages[1]["content"][1]["type"], "tool_use");
    Ok(())
}

#[test]
fn thinking_replay_refuses_missing_foreign_model_or_unbound_source() -> Result<()> {
    let mut request = request();
    assert!(apply(&mut build_messages_request(&request, 32768)?, &request).is_err());
    let mut state = into_state(
        "deepseek-flash",
        vec![json!({"type":"thinking","thinking":"source","signature":""})],
    )?;
    state.message_id = Some(request.messages[1].id.clone());
    request.continuation_states.push(state);
    request.model_name = "another-model".to_owned();
    assert!(apply(&mut build_messages_request(&request, 32768)?, &request).is_err());
    request.model_name = "deepseek-flash".to_owned();
    request.continuation_states[0].message_id = Some("another-message".to_owned());
    assert!(apply(&mut build_messages_request(&request, 32768)?, &request).is_err());
    Ok(())
}

#[test]
fn interrupted_thinking_stream_never_publishes_replay_state() -> Result<()> {
    let mut mapper =
        StreamMapper::new(None).with_thinking_replay(Some("deepseek-flash".to_owned()));
    mapper.map_envelope(serde_json::from_value(json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"partial","signature":""}}))?)?;
    assert!(
        mapper.finish().is_err(),
        "incomplete thinking cannot certify a continuation"
    );
    Ok(())
}
