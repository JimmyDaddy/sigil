use sigil_kernel::{HostedEvidence, HostedToolKind, ProviderChunk};

use super::*;
use crate::hosted_search::GeminiHostedInvocation;
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
fn stream_mapper_preserves_function_call_thought_signature() -> anyhow::Result<()> {
    let mut mapper = StreamMapper::new();

    let chunks = map_json(
        &mut mapper,
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"id":"call-1","name":"read_file","args":{"path":"src/lib.rs"}},"thoughtSignature":"sig-1"}]}}]}"#,
    )?;

    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::ContinuationState(state)
            if state.provider_name == "gemini"
                && state.state_kind == GEMINI_THOUGHT_SIGNATURE_STATE_KIND
                && state.opaque_blob["tool_call_id"] == "call-1"
                && state.opaque_blob["thought_signature"] == "sig-1"
    )));
    Ok(())
}

#[test]
fn stream_mapper_accepts_normal_finish_reasons() -> anyhow::Result<()> {
    let mut mapper = StreamMapper::new();

    let chunks = map_json(
        &mut mapper,
        r#"{"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":"done"}]}}]}"#,
    )?;

    assert!(matches!(
        chunks.first(),
        Some(ProviderChunk::TextDelta(text)) if text == "done"
    ));
    Ok(())
}

#[test]
fn stream_mapper_errors_on_abnormal_finish_reason_with_details() {
    let mut mapper = StreamMapper::new();

    let error = map_json(
        &mut mapper,
        r#"{"candidates":[{"finishReason":"MALFORMED_FUNCTION_CALL","finishMessage":"bad args"}]}"#,
    )
    .expect_err("malformed function call should fail");

    assert!(error.to_string().contains("MALFORMED_FUNCTION_CALL"));
    assert!(error.to_string().contains("bad args"));

    let error = map_json(
        &mut mapper,
        r#"{"candidates":[{"finishReason":"SAFETY","safetyRatings":[{"category":"HARM_CATEGORY_DANGEROUS_CONTENT","probability":"HIGH","blocked":true}]}]}"#,
    )
    .expect_err("safety finish should fail");

    assert!(error.to_string().contains("SAFETY"));
    assert!(
        error
            .to_string()
            .contains("HARM_CATEGORY_DANGEROUS_CONTENT")
    );

    let error = map_json(
        &mut mapper,
        r#"{"candidates":[{"finishReason":"SAFETY","finishMessage":"blocked","safetyRatings":[{"category":"HARM_CATEGORY_HATE_SPEECH","probability":"MEDIUM","blocked":false}]}]}"#,
    )
    .expect_err("safety finish should include message and ratings");

    assert!(error.to_string().contains("blocked; safety="));
    assert!(error.to_string().contains("HARM_CATEGORY_HATE_SPEECH"));

    let error = map_json(
        &mut mapper,
        r#"{"candidates":[{"finishReason":"RECITATION"}]}"#,
    )
    .expect_err("abnormal finish without details should fail");

    assert_eq!(
        error.to_string(),
        "Gemini response finished abnormally: RECITATION"
    );
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

fn hosted_mapper() -> StreamMapper {
    StreamMapper::with_hosted(GeminiHostedInvocation {
        authorization_id: "auth-gemini".to_owned(),
        invocation_id: "invocation-gemini".to_owned(),
    })
}

#[test]
fn hosted_search_mapper_accumulates_queries_sources_and_unicode_citations() -> anyhow::Result<()> {
    let mut mapper = hosted_mapper();
    let mut chunks = map_json(
        &mut mapper,
        r#"{"candidates":[{"index":0,"content":{"parts":[{"text":"猫🙂"}]}}]}"#,
    )?;
    chunks.extend(map_json(
        &mut mapper,
        r#"{"candidates":[{"index":0,"content":{"parts":[{"text":" grounded"}]},"groundingMetadata":{"webSearchQueries":["first raw query","second raw query"],"groundingChunks":[{"web":{"uri":"https://one.example/path?token=raw","title":"One"}},{"web":{"uri":"https://two.example","title":"Two"}}],"groundingSupports":[{"segment":{"partIndex":0,"startIndex":0,"endIndex":7,"text":"猫🙂"},"groundingChunkIndices":[0,1]}]}}]}"#,
    )?);

    assert!(matches!(
        chunks.get(2),
        Some(ProviderChunk::HostedToolStarted {
            authorization_id,
            invocation_id,
            kind: HostedToolKind::WebSearch,
        }) if authorization_id == "auth-gemini" && invocation_id == "invocation-gemini"
    ));
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| matches!(
                chunk,
                ProviderChunk::HostedEvidence {
                    evidence: HostedEvidence::QueryObserved(_),
                    ..
                }
            ))
            .count(),
        2
    );
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
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| matches!(
                chunk,
                ProviderChunk::HostedEvidence {
                    evidence: HostedEvidence::Citation(_),
                    ..
                }
            ))
            .count(),
        2
    );
    let debug = format!("{chunks:?}");
    assert!(!debug.contains("first raw query"));
    assert!(!debug.contains("token=raw"));
    Ok(())
}

#[test]
fn hosted_search_mapper_keeps_grounding_chunk_indices_across_stream_responses() -> anyhow::Result<()>
{
    let mut mapper = hosted_mapper();
    let first = map_json(
        &mut mapper,
        r#"{"candidates":[{"index":0,"content":{"parts":[{"text":"one "}]},"groundingMetadata":{"webSearchQueries":["query one"],"groundingChunks":[{"web":{"uri":"https://one.example"}}],"groundingSupports":[]}}]}"#,
    )?;
    assert_eq!(
        first
            .iter()
            .filter(|chunk| matches!(
                chunk,
                ProviderChunk::HostedEvidence {
                    evidence: HostedEvidence::Source(_),
                    ..
                }
            ))
            .count(),
        1
    );

    let second = map_json(
        &mut mapper,
        r#"{"candidates":[{"index":0,"content":{"parts":[{"text":"two"}]},"groundingMetadata":{"webSearchQueries":["query two"],"groundingChunks":[{"web":{"uri":"https://two.example"}}],"groundingSupports":[{"segment":{"partIndex":0,"startIndex":0,"endIndex":7,"text":"one two"},"groundingChunkIndices":[0,1]}]}}]}"#,
    )?;
    let citations = second
        .iter()
        .filter_map(|chunk| match chunk {
            ProviderChunk::HostedEvidence {
                evidence: HostedEvidence::Citation(citation),
                ..
            } => Some(citation),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(citations.len(), 2);
    assert!(citations.iter().all(|citation| citation.start_byte() == 0));
    assert!(citations.iter().all(|citation| citation.end_byte() == 7));
    Ok(())
}

#[test]
fn hosted_search_mapper_distinguishes_not_used_and_no_results() -> anyhow::Result<()> {
    let mut not_used = hosted_mapper();
    let chunks = map_json(
        &mut not_used,
        r#"{"candidates":[{"content":{"parts":[{"text":"answer"}]}}]}"#,
    )?;
    assert!(
        !chunks
            .iter()
            .any(|chunk| matches!(chunk, ProviderChunk::HostedToolStarted { .. }))
    );

    let mut no_results = hosted_mapper();
    let chunks = map_json(
        &mut no_results,
        r#"{"candidates":[{"content":{"parts":[{"text":"no result"}]},"groundingMetadata":{"webSearchQueries":["query"],"groundingChunks":[],"groundingSupports":[]}}]}"#,
    )?;
    assert!(
        chunks
            .iter()
            .any(|chunk| matches!(chunk, ProviderChunk::HostedToolStarted { .. }))
    );
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::HostedEvidence {
            evidence: HostedEvidence::QueryObserved(_),
            ..
        }
    )));
    assert!(
        !chunks
            .iter()
            .any(|chunk| matches!(chunk, ProviderChunk::HostedToolFailed { .. }))
    );
    Ok(())
}

#[test]
fn non_hosted_search_mapper_rejects_unexpected_grounding_metadata() {
    let mut mapper = StreamMapper::new();
    let error = map_json(
        &mut mapper,
        r#"{"candidates":[{"groundingMetadata":{"webSearchQueries":["raw query"]}}]}"#,
    )
    .expect_err("unexpected grounding must fail");
    assert!(error.to_string().contains("without hosted search"));
    assert!(!format!("{error:?}").contains("raw query"));
}
