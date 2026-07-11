use std::collections::BTreeMap;

use sigil_kernel::{HostedEvidence, ProviderChunk, WebSearchFailureClass};

use super::*;
use crate::models::AnthropicStreamEnvelope;

fn map_json(mapper: &mut StreamMapper, raw: &str) -> anyhow::Result<Vec<ProviderChunk>> {
    let envelope: AnthropicStreamEnvelope = serde_json::from_str(raw)?;
    mapper.map_envelope(envelope)
}

fn hosted_mapper() -> (
    StreamMapper,
    crate::hosted_search::AnthropicHostedContinuationStore,
) {
    let store = crate::hosted_search::AnthropicHostedContinuationStore::default();
    let mapper = StreamMapper::new(Some(crate::hosted_search::AnthropicHostedStreamContext {
        authorization_id: "authorization-1".to_owned(),
        continuation_store: store.clone(),
        prior_invocations: BTreeMap::new(),
    }));
    (mapper, store)
}

#[test]
fn stream_mapper_reconstructs_text_tool_use_usage_and_done() -> anyhow::Result<()> {
    let mut mapper = StreamMapper::new(None);
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
    let mut mapper = StreamMapper::new(None);

    let error = map_json(
        &mut mapper,
        r#"{"type":"error","error":{"type":"overloaded_error","message":"try later"}}"#,
    )
    .expect_err("error event should fail");

    assert!(error.to_string().contains("overloaded_error: try later"));
}

#[test]
fn finish_emits_pending_usage_without_done() -> anyhow::Result<()> {
    let mut mapper = StreamMapper::new(None);

    let chunks = map_json(
        &mut mapper,
        r#"{"type":"message_start","message":{"usage":{"input_tokens":3,"output_tokens":2}}}"#,
    )?;
    assert!(chunks.is_empty());

    let chunks = mapper.finish()?;
    assert!(matches!(
        chunks.first(),
        Some(ProviderChunk::Usage(usage)) if usage.prompt_tokens == 3 && usage.completion_tokens == 2
    ));
    Ok(())
}

#[test]
fn stream_mapper_covers_block_start_and_delta_edges() -> anyhow::Result<()> {
    let mut mapper = StreamMapper::new(None);
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
    let mut mapper = StreamMapper::new(None);

    let error = map_json(
        &mut mapper,
        r#"{"type":"error","error":{"type":"api_error","message":""}}"#,
    )
    .expect_err("error event should fail");

    assert_eq!(error.to_string(), "Anthropic stream error: api_error");
}

#[test]
fn hosted_search_mapper_normalizes_direct_search_citation_usage_and_continuation()
-> anyhow::Result<()> {
    let (mut mapper, store) = hosted_mapper();
    let mut chunks = Vec::new();
    for raw in [
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srvtoolu_1","name":"web_search","input":{}}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"latest rust\"}"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"web_search_tool_result","tool_use_id":"srvtoolu_1","content":[{"type":"web_search_result","url":"https://docs.example.com/rust","title":"Rust docs","encrypted_content":"encrypted-secret","page_age":"2026-07-11"}]}}"#,
        r#"{"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"Rust 🦀"}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"citations_delta","citation":{"type":"web_search_result_location","url":"https://docs.example.com/rust","title":"Rust docs","encrypted_index":"encrypted-index-secret","cited_text":"source excerpt"}}}"#,
        r#"{"type":"content_block_stop","index":2}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":12,"output_tokens":4,"cache_read_input_tokens":2,"server_tool_use":{"web_search_requests":1}}}"#,
        r#"{"type":"message_stop"}"#,
    ] {
        chunks.extend(map_json(&mut mapper, raw)?);
    }

    assert!(matches!(
        chunks.first(),
        Some(ProviderChunk::HostedToolStarted { authorization_id, invocation_id, .. })
            if authorization_id == "authorization-1" && invocation_id == "srvtoolu_1"
    ));
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::HostedEvidence { invocation_id, evidence: HostedEvidence::QueryObserved(query), .. }
            if invocation_id == "srvtoolu_1" && query.expose_secret() == "latest rust"
    )));
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::HostedEvidence { invocation_id, evidence: HostedEvidence::Source(source), .. }
            if invocation_id == "srvtoolu_1"
                && source.raw_url() == "https://docs.example.com/rust"
                && source.raw_title() == Some("Rust docs")
    )));
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::HostedEvidence { invocation_id, evidence: HostedEvidence::Citation(citation), .. }
            if invocation_id == "srvtoolu_1"
                && citation.start_byte() == 0
                && citation.end_byte() == "Rust 🦀".len()
    )));
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::HostedRequestUsage {
            observed_uses: 1,
            ..
        }
    )));
    let mut state = chunks
        .iter()
        .find_map(|chunk| match chunk {
            ProviderChunk::ContinuationState(state) => Some(state.clone()),
            _ => None,
        })
        .expect("hosted turn should retain a continuation");
    let durable = serde_json::to_string(&state)?;
    for secret in [
        "latest rust",
        "encrypted-secret",
        "encrypted-index-secret",
        "https://docs.example.com/rust",
    ] {
        assert!(!durable.contains(secret));
    }
    state.message_id = Some("assistant-1".to_owned());
    assert!(matches!(
        store.resolve_for_message(&[state], "assistant-1")?,
        crate::hosted_search::ContinuationResolution::Live(blocks)
            if blocks.iter().any(|block| block.to_string().contains("encrypted-secret"))
    ));
    Ok(())
}

#[test]
fn hosted_search_mapper_allows_an_authorized_request_to_finish_without_search_use()
-> anyhow::Result<()> {
    let (mut mapper, _) = hosted_mapper();
    let mut chunks = Vec::new();
    for raw in [
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"No search was needed."}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":4,"output_tokens":5}}"#,
        r#"{"type":"message_stop"}"#,
    ] {
        chunks.extend(map_json(&mut mapper, raw)?);
    }

    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::TextDelta(text) if text == "No search was needed."
    )));
    assert!(!chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::HostedToolStarted { .. }
            | ProviderChunk::HostedEvidence { .. }
            | ProviderChunk::HostedToolFailed { .. }
            | ProviderChunk::HostedRequestUsage { .. }
    )));
    assert!(matches!(chunks.last(), Some(ProviderChunk::Done)));
    Ok(())
}

#[test]
fn hosted_search_mapper_handles_multiple_empty_and_body_error_results() -> anyhow::Result<()> {
    let (mut mapper, _) = hosted_mapper();
    let mut chunks = Vec::new();
    for raw in [
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srvtoolu_a","name":"web_search","input":{"query":"a"}}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"web_search_tool_result","tool_use_id":"srvtoolu_a","content":[]}}"#,
        r#"{"type":"content_block_start","index":2,"content_block":{"type":"server_tool_use","id":"srvtoolu_b","name":"web_search","input":{"query":"b"}}}"#,
        r#"{"type":"content_block_stop","index":2}"#,
        r#"{"type":"content_block_start","index":3,"content_block":{"type":"web_search_tool_result","tool_use_id":"srvtoolu_b","content":{"type":"web_search_tool_result_error","error_code":"max_uses_exceeded"}}}"#,
        r#"{"type":"content_block_start","index":4,"content_block":{"type":"server_tool_use","id":"srvtoolu_c","name":"web_search","input":{"query":"c"}}}"#,
        r#"{"type":"content_block_stop","index":4}"#,
        r#"{"type":"content_block_start","index":5,"content_block":{"type":"web_search_tool_result","tool_use_id":"srvtoolu_c","content":{"type":"web_search_tool_result_error","error_code":"invalid_input"}}}"#,
    ] {
        chunks.extend(map_json(&mut mapper, raw)?);
    }
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| matches!(chunk, ProviderChunk::HostedToolStarted { .. }))
            .count(),
        3
    );
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::HostedToolFailed {
            invocation_id,
            failure_class: WebSearchFailureClass::BudgetExhausted,
            ..
        } if invocation_id == "srvtoolu_b"
    )));
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::HostedToolFailed {
            invocation_id,
            failure_class: WebSearchFailureClass::ProtocolError,
            ..
        } if invocation_id == "srvtoolu_c"
    )));
    assert!(!chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::HostedEvidence {
            evidence: HostedEvidence::Source(_),
            ..
        }
    )));
    Ok(())
}

#[test]
fn hosted_search_mapper_preserves_multiple_search_citation_ownership_and_utf8_spans()
-> anyhow::Result<()> {
    let (mut mapper, _) = hosted_mapper();
    let mut chunks = Vec::new();
    for raw in [
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srvtoolu_a","name":"web_search","input":{"query":"a"}}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"web_search_tool_result","tool_use_id":"srvtoolu_a","content":[{"type":"web_search_result","url":"https://a.example.com","title":"A","encrypted_content":"enc-a"}]}}"#,
        r#"{"type":"content_block_start","index":2,"content_block":{"type":"server_tool_use","id":"srvtoolu_b","name":"web_search","input":{"query":"b"}}}"#,
        r#"{"type":"content_block_stop","index":2}"#,
        r#"{"type":"content_block_start","index":3,"content_block":{"type":"web_search_tool_result","tool_use_id":"srvtoolu_b","content":[{"type":"web_search_result","url":"https://b.example.com","title":"B","encrypted_content":"enc-b"}]}}"#,
        r#"{"type":"content_block_start","index":4,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":4,"delta":{"type":"text_delta","text":"alpha"}}"#,
        r#"{"type":"content_block_delta","index":4,"delta":{"type":"citations_delta","citation":{"type":"web_search_result_location","url":"https://a.example.com","title":"A","encrypted_index":"idx-a","cited_text":"source a"}}}"#,
        r#"{"type":"content_block_delta","index":4,"delta":{"type":"text_delta","text":"βeta"}}"#,
        r#"{"type":"content_block_delta","index":4,"delta":{"type":"citations_delta","citation":{"type":"web_search_result_location","url":"https://b.example.com","title":"B","encrypted_index":"idx-b","cited_text":"source b"}}}"#,
        r#"{"type":"content_block_stop","index":4}"#,
        r#"{"type":"message_stop"}"#,
    ] {
        chunks.extend(map_json(&mut mapper, raw)?);
    }
    let citations = chunks
        .iter()
        .filter_map(|chunk| match chunk {
            ProviderChunk::HostedEvidence {
                invocation_id,
                evidence: HostedEvidence::Citation(citation),
                ..
            } => Some((
                invocation_id.as_str(),
                citation.start_byte(),
                citation.end_byte(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        citations,
        vec![("srvtoolu_a", 0, 5), ("srvtoolu_b", 5, 5 + "βeta".len())]
    );
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::HostedEvidence {
            invocation_id,
            evidence: HostedEvidence::Source(source),
            ..
        } if invocation_id == "srvtoolu_b" && source.raw_url() == "https://b.example.com"
    )));
    Ok(())
}

#[test]
fn hosted_search_mapper_drops_unknown_and_ambiguous_url_citations() -> anyhow::Result<()> {
    let (mut mapper, _) = hosted_mapper();
    let mut chunks = Vec::new();
    for raw in [
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srvtoolu_a","name":"web_search","input":{"query":"a"}}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"web_search_tool_result","tool_use_id":"srvtoolu_a","content":[{"type":"web_search_result","url":"https://same.example.com","title":"A","encrypted_content":"enc-a"},{"type":"web_search_result","url":"https://same.example.com","title":"A2","encrypted_content":"enc-a2"}]}}"#,
        r#"{"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"ambiguous"}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"citations_delta","citation":{"type":"web_search_result_location","url":"https://same.example.com","title":"A","encrypted_index":"idx-a","cited_text":"source"}}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"unknown"}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"citations_delta","citation":{"type":"web_search_result_location","url":"https://unknown.example.com","title":"Unknown","encrypted_index":"idx-u","cited_text":"source"}}}"#,
        r#"{"type":"content_block_stop","index":2}"#,
        r#"{"type":"message_stop"}"#,
    ] {
        chunks.extend(map_json(&mut mapper, raw)?);
    }
    assert!(!chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::HostedEvidence {
            evidence: HostedEvidence::Citation(_),
            ..
        }
    )));
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| matches!(
                chunk,
                ProviderChunk::HostedEvidence {
                    evidence: HostedEvidence::Source(_),
                    ..
                }
            ))
            .count(),
        2
    );
    Ok(())
}

#[test]
fn hosted_search_mapper_keeps_mixed_client_and_server_tools_separate() -> anyhow::Result<()> {
    let (mut mapper, store) = hosted_mapper();
    let mut chunks = Vec::new();
    for raw in [
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srvtoolu_1","name":"web_search","input":{"query":"status"}}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"README.md\"}"}}"#,
        r#"{"type":"content_block_stop","index":1}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}"#,
        r#"{"type":"message_stop"}"#,
    ] {
        chunks.extend(map_json(&mut mapper, raw)?);
    }
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::ToolCallComplete(call)
            if call.id == "toolu_1" && call.name == "read_file"
    )));
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| matches!(chunk, ProviderChunk::HostedToolStarted { .. }))
            .count(),
        1
    );
    let mut state = chunks
        .iter()
        .find_map(|chunk| match chunk {
            ProviderChunk::ContinuationState(state) => Some(state.clone()),
            _ => None,
        })
        .expect("mixed turn should retain exact blocks");
    assert_eq!(state.opaque_blob["continuation_reason"], "mixed_tool_use");
    state.message_id = Some("assistant-1".to_owned());
    assert!(matches!(
        store.resolve_for_message(&[state], "assistant-1")?,
        crate::hosted_search::ContinuationResolution::Live(blocks)
            if blocks.iter().any(|block| block["type"] == "server_tool_use")
                && blocks.iter().any(|block| block["type"] == "tool_use")
    ));
    Ok(())
}

#[test]
fn hosted_search_mapper_marks_pause_turn_and_rejects_unsafe_eof() -> anyhow::Result<()> {
    let (mut mapper, _) = hosted_mapper();
    map_json(
        &mut mapper,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srvtoolu_1","name":"web_search","input":{"query":"long research"}}}"#,
    )?;
    map_json(&mut mapper, r#"{"type":"content_block_stop","index":0}"#)?;
    map_json(
        &mut mapper,
        r#"{"type":"message_delta","delta":{"stop_reason":"pause_turn"},"usage":{"output_tokens":1}}"#,
    )?;
    let chunks = map_json(&mut mapper, r#"{"type":"message_stop"}"#)?;
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::ContinuationState(state)
            if state.opaque_blob["continuation_reason"] == "pause_turn"
    )));

    let (mut disconnected, _) = hosted_mapper();
    map_json(
        &mut disconnected,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srvtoolu_2","name":"web_search","input":{}}}"#,
    )?;
    assert!(
        disconnected
            .finish()
            .expect_err("hosted EOF must fail")
            .to_string()
            .contains("before message_stop")
    );
    Ok(())
}
