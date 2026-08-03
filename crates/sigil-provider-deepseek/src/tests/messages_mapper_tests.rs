use serde_json::json;
use sigil_kernel::{HostedEvidence, HostedToolKind, ProviderChunk, WebSearchFailureClass};

use super::MessagesStreamMapper;
use crate::messages_continuation::{
    DEEPSEEK_HOSTED_CONTINUATION_KIND, DeepSeekHostedContinuationStore, DeepSeekHostedStreamContext,
};
use crate::messages_models::{
    DeepSeekCitation, DeepSeekContentBlock, DeepSeekContentBlockDelta, DeepSeekMessageDelta,
    DeepSeekMessageStart, DeepSeekMessagesEnvelope, DeepSeekServerToolUsage, DeepSeekUsage,
    DeepSeekWebSearchResult, DeepSeekWebSearchToolResultContent, DeepSeekWebSearchToolResultError,
};

fn context(authorization_id: &str) -> DeepSeekHostedStreamContext {
    DeepSeekHostedStreamContext {
        authorization_id: authorization_id.to_owned(),
        continuation_store: DeepSeekHostedContinuationStore::default(),
        prior_invocations: Default::default(),
    }
}

fn search_result(url: &str, title: &str) -> DeepSeekWebSearchResult {
    DeepSeekWebSearchResult {
        r#type: "web_search_result".to_owned(),
        url: url.to_owned(),
        title: title.to_owned(),
        encrypted_content: "encrypted".to_owned(),
        page_age: None,
    }
}

fn usage(input: u64, output: u64, web_search_requests: u32) -> DeepSeekUsage {
    DeepSeekUsage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        server_tool_use: Some(DeepSeekServerToolUsage {
            web_search_requests,
        }),
    }
}

fn collect(
    envelopes: Vec<DeepSeekMessagesEnvelope>,
    mapper: &mut MessagesStreamMapper,
) -> Vec<ProviderChunk> {
    let mut all = Vec::new();
    for envelope in envelopes {
        all.extend(mapper.map_envelope(envelope).expect("envelope maps"));
    }
    all
}

#[test]
fn full_hosted_turn_emits_started_query_sources_citations_usage_and_continuation() {
    let mut mapper = MessagesStreamMapper::new(Some(context("auth-1")));
    let chunks = collect(
        vec![
            DeepSeekMessagesEnvelope::MessageStart {
                message: DeepSeekMessageStart {
                    usage: Some(usage(12, 0, 0)),
                },
            },
            DeepSeekMessagesEnvelope::ContentBlockStart {
                index: 0,
                content_block: DeepSeekContentBlock::Text {
                    text: String::new(),
                },
            },
            DeepSeekMessagesEnvelope::ContentBlockDelta {
                index: 0,
                delta: DeepSeekContentBlockDelta::TextDelta {
                    text: "Finding: ".to_owned(),
                },
            },
            DeepSeekMessagesEnvelope::ContentBlockStart {
                index: 1,
                content_block: DeepSeekContentBlock::ServerToolUse {
                    id: "invoke-1".to_owned(),
                    name: "web_search".to_owned(),
                    input: json!({"query": "deepseek api"}),
                },
            },
            DeepSeekMessagesEnvelope::ContentBlockStop { index: 1 },
            DeepSeekMessagesEnvelope::ContentBlockStart {
                index: 2,
                content_block: DeepSeekContentBlock::WebSearchToolResult {
                    tool_use_id: "invoke-1".to_owned(),
                    content: DeepSeekWebSearchToolResultContent::Results(vec![search_result(
                        "https://example.com/a",
                        "A",
                    )]),
                },
            },
            DeepSeekMessagesEnvelope::ContentBlockStop { index: 2 },
            DeepSeekMessagesEnvelope::ContentBlockDelta {
                index: 0,
                delta: DeepSeekContentBlockDelta::CitationsDelta {
                    citation: DeepSeekCitation::WebSearchResultLocation {
                        url: "https://example.com/a".to_owned(),
                        title: Some("A".to_owned()),
                        encrypted_index: "0".to_owned(),
                        cited_text: "Finding".to_owned(),
                    },
                },
            },
            DeepSeekMessagesEnvelope::ContentBlockDelta {
                index: 0,
                delta: DeepSeekContentBlockDelta::TextDelta {
                    text: "answer".to_owned(),
                },
            },
            DeepSeekMessagesEnvelope::MessageDelta {
                delta: DeepSeekMessageDelta {
                    stop_reason: Some("end_turn".to_owned()),
                },
                usage: Some(usage(20, 6, 1)),
            },
            DeepSeekMessagesEnvelope::MessageStop,
        ],
        &mut mapper,
    );

    assert!(chunks.iter().any(|c| matches!(
        c,
        ProviderChunk::HostedToolStarted { authorization_id, invocation_id, kind }
            if authorization_id == "auth-1"
                && invocation_id == "invoke-1"
                && *kind == HostedToolKind::WebSearch
    )));
    assert!(chunks.iter().any(|c| matches!(
        c,
        ProviderChunk::HostedEvidence { invocation_id, evidence: HostedEvidence::QueryObserved(query), .. }
            if invocation_id == "invoke-1" && query.expose_secret() == "deepseek api"
    )));
    assert!(chunks.iter().any(|c| matches!(
        c,
        ProviderChunk::HostedEvidence { evidence: HostedEvidence::Source(source), .. }
            if source.raw_url() == "https://example.com/a"
                && source.raw_title() == Some("A")
                && source.rank() == Some(0)
    )));
    assert!(chunks.iter().any(|c| matches!(
        c,
        ProviderChunk::HostedEvidence { evidence: HostedEvidence::Citation(citation), .. }
            if citation.provider_source_id() == "https://example.com/a"
                && citation.start_byte() == 0
                && citation.end_byte() == 9
    )));
    assert!(chunks.iter().any(|c| matches!(
        c,
        ProviderChunk::HostedRequestUsage { observed_uses, .. } if *observed_uses == 1
    )));
    assert!(chunks.iter().any(|c| matches!(
        c,
        ProviderChunk::ContinuationState(state)
            if state.state_kind == DEEPSEEK_HOSTED_CONTINUATION_KIND
    )));
    assert!(chunks.iter().any(|c| matches!(c, ProviderChunk::Done)));
    assert!(
        chunks
            .iter()
            .any(|c| matches!(c, ProviderChunk::TextDelta(text) if text == "Finding: "))
    );
    assert!(
        chunks
            .iter()
            .any(|c| matches!(c, ProviderChunk::TextDelta(text) if text == "answer"))
    );
    let usages = chunks
        .iter()
        .filter(|c| matches!(c, ProviderChunk::Usage(_)))
        .count();
    assert_eq!(usages, 1, "usage must be emitted exactly once");
}

#[test]
fn search_error_maps_to_hosted_tool_failed() {
    let mut mapper = MessagesStreamMapper::new(Some(context("auth-1")));
    let chunks = collect(
        vec![
            DeepSeekMessagesEnvelope::ContentBlockStart {
                index: 0,
                content_block: DeepSeekContentBlock::ServerToolUse {
                    id: "invoke-1".to_owned(),
                    name: "web_search".to_owned(),
                    input: json!({"query": "q"}),
                },
            },
            DeepSeekMessagesEnvelope::ContentBlockStop { index: 0 },
            DeepSeekMessagesEnvelope::ContentBlockStart {
                index: 1,
                content_block: DeepSeekContentBlock::WebSearchToolResult {
                    tool_use_id: "invoke-1".to_owned(),
                    content: DeepSeekWebSearchToolResultContent::Error(
                        DeepSeekWebSearchToolResultError {
                            r#type: "web_search_error".to_owned(),
                            error_code: "max_uses_exceeded".to_owned(),
                        },
                    ),
                },
            },
            DeepSeekMessagesEnvelope::ContentBlockStop { index: 1 },
            DeepSeekMessagesEnvelope::MessageStop,
        ],
        &mut mapper,
    );
    assert!(chunks.iter().any(|c| matches!(
        c,
        ProviderChunk::HostedToolFailed { invocation_id, failure_class, .. }
            if invocation_id == "invoke-1"
                && *failure_class == WebSearchFailureClass::BudgetExhausted
    )));
}

#[test]
fn client_tool_use_completes_alongside_hosted_declaration() {
    let mut mapper = MessagesStreamMapper::new(Some(context("auth-1")));
    let chunks = collect(
        vec![
            DeepSeekMessagesEnvelope::ContentBlockStart {
                index: 0,
                content_block: DeepSeekContentBlock::ToolUse {
                    id: "client-1".to_owned(),
                    name: "read_file".to_owned(),
                    input: json!({"path": "a.rs"}),
                },
            },
            DeepSeekMessagesEnvelope::ContentBlockStop { index: 0 },
            DeepSeekMessagesEnvelope::MessageStop,
        ],
        &mut mapper,
    );
    assert!(chunks.iter().any(|c| matches!(
        c,
        ProviderChunk::ToolCallComplete(call) if call.id == "client-1" && call.name == "read_file"
    )));
    assert!(
        chunks
            .iter()
            .filter(|c| matches!(c, ProviderChunk::ContinuationState(_)))
            .count()
            == 0,
        "no hosted continuation is emitted without a server tool invocation"
    );
}

#[test]
fn unsupported_server_tool_name_fails_closed() {
    let mut mapper = MessagesStreamMapper::new(Some(context("auth-1")));
    let error = mapper
        .map_envelope(DeepSeekMessagesEnvelope::ContentBlockStart {
            index: 0,
            content_block: DeepSeekContentBlock::ServerToolUse {
                id: "invoke-1".to_owned(),
                name: "other".to_owned(),
                input: json!({}),
            },
        })
        .expect_err("unsupported server tool must fail");
    assert!(
        error
            .to_string()
            .contains("unsupported DeepSeek server tool")
    );
}

#[test]
fn server_tool_use_without_hosted_context_fails_closed() {
    let mut mapper = MessagesStreamMapper::new(None);
    let error = mapper
        .map_envelope(DeepSeekMessagesEnvelope::ContentBlockStart {
            index: 0,
            content_block: DeepSeekContentBlock::ServerToolUse {
                id: "invoke-1".to_owned(),
                name: "web_search".to_owned(),
                input: json!({}),
            },
        })
        .expect_err("server web search without hosted context must fail");
    assert!(error.to_string().contains("non-hosted request"));
}

#[test]
fn hosted_stream_ending_without_message_stop_fails_closed() {
    let mut mapper = MessagesStreamMapper::new(Some(context("auth-1")));
    let chunks = collect(
        vec![DeepSeekMessagesEnvelope::ContentBlockStart {
            index: 0,
            content_block: DeepSeekContentBlock::ServerToolUse {
                id: "invoke-1".to_owned(),
                name: "web_search".to_owned(),
                input: json!({"query": "q"}),
            },
        }],
        &mut mapper,
    );
    assert!(!chunks.is_empty());
    let error = mapper
        .finish()
        .expect_err("hosted stream without message_stop must fail");
    assert!(error.to_string().contains("before message_stop"));
}

#[test]
fn search_result_without_matching_invocation_fails_closed() {
    let mut mapper = MessagesStreamMapper::new(Some(context("auth-1")));
    let error = mapper
        .map_envelope(DeepSeekMessagesEnvelope::ContentBlockStart {
            index: 0,
            content_block: DeepSeekContentBlock::WebSearchToolResult {
                tool_use_id: "unknown-invocation".to_owned(),
                content: DeepSeekWebSearchToolResultContent::Results(vec![search_result(
                    "https://example.com/a",
                    "A",
                )]),
            },
        })
        .expect_err("unmatched search result must fail");
    assert!(error.to_string().contains("no matching server-tool use"));
}
