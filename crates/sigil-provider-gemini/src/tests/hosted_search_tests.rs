use sigil_kernel::HostedEvidence;

use super::{
    GeminiGroundingAccumulator, gemini_hosted_custom_tools_supported,
    gemini_hosted_web_search_supported,
};
use crate::models::GeminiGroundingMetadata;

#[test]
fn exact_model_matrix_accepts_documented_models_and_resource_prefix() {
    for model in [
        "gemini-3.5-flash",
        "gemini-3.1-flash-image-preview",
        "gemini-3.1-pro-preview",
        "gemini-3-pro-image-preview",
        "gemini-3-flash-preview",
        "gemini-2.5-pro",
        "gemini-2.5-flash",
        "gemini-2.5-flash-lite",
        "gemini-2.0-flash",
        "models/gemini-2.5-flash",
    ] {
        assert!(
            gemini_hosted_web_search_supported(model),
            "expected {model} to support hosted search"
        );
    }
}

#[test]
fn grounding_accumulator_maps_multiple_queries_sources_and_unicode_byte_spans() -> anyhow::Result<()>
{
    let mut accumulator = GeminiGroundingAccumulator::new();
    accumulator.record_text(0, 0, "猫🙂")?;
    accumulator.record_text(0, 0, " grounded")?;
    let metadata: GeminiGroundingMetadata = serde_json::from_value(serde_json::json!({
        "webSearchQueries": ["first query", "second query"],
        "groundingChunks": [
            {"web": {"uri": "https://one.example/path?q=raw", "title": "One"}},
            {"web": {"uri": "https://two.example", "title": "Two"}}
        ],
        "groundingSupports": [{
            "segment": {
                "partIndex": 0,
                "startIndex": 0,
                "endIndex": 7,
                "text": "猫🙂"
            },
            "groundingChunkIndices": [0, 1]
        }]
    }))?;

    let evidence = accumulator.map_metadata(0, metadata)?;

    assert_eq!(
        evidence
            .iter()
            .filter(|item| matches!(item, HostedEvidence::QueryObserved(_)))
            .count(),
        2
    );
    assert_eq!(
        evidence
            .iter()
            .filter(|item| matches!(item, HostedEvidence::Source(_)))
            .count(),
        2
    );
    let citations = evidence
        .iter()
        .filter_map(|item| match item {
            HostedEvidence::Citation(citation) => Some(citation),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(citations.len(), 2);
    assert!(citations.iter().all(|item| item.start_byte() == 0));
    assert!(citations.iter().all(|item| item.end_byte() == 7));
    Ok(())
}

#[test]
fn grounding_accumulator_drops_invalid_claim_support_but_keeps_sources() -> anyhow::Result<()> {
    let mut accumulator = GeminiGroundingAccumulator::new();
    accumulator.record_text(0, 0, "猫🙂 grounded")?;
    let metadata: GeminiGroundingMetadata = serde_json::from_value(serde_json::json!({
        "groundingChunks": [
            {"web": {"uri": "https://one.example", "title": "One"}},
            {"web": {"uri": "https://two.example", "title": "Two"}}
        ],
        "groundingSupports": [
            {
                "segment": {"partIndex": 0, "startIndex": 1, "endIndex": 7},
                "groundingChunkIndices": [0]
            },
            {
                "segment": {"partIndex": 0, "startIndex": 0, "endIndex": 7, "text": "mismatch"},
                "groundingChunkIndices": [1]
            },
            {
                "segment": {"partIndex": 0, "startIndex": 0, "endIndex": 7, "text": "猫🙂"},
                "groundingChunkIndices": [99]
            }
        ]
    }))?;

    let evidence = accumulator.map_metadata(0, metadata)?;

    assert_eq!(
        evidence
            .iter()
            .filter(|item| matches!(item, HostedEvidence::Source(_)))
            .count(),
        2
    );
    assert!(
        !evidence
            .iter()
            .any(|item| matches!(item, HostedEvidence::Citation(_)))
    );
    Ok(())
}

#[test]
fn grounding_accumulator_maps_support_across_adjacent_text_deltas() -> anyhow::Result<()> {
    let mut accumulator = GeminiGroundingAccumulator::new();
    accumulator.record_text(0, 0, "Rust ")?;
    accumulator.record_text(0, 0, "works")?;
    let metadata: GeminiGroundingMetadata = serde_json::from_value(serde_json::json!({
        "groundingChunks": [{"web": {"uri": "https://example.com"}}],
        "groundingSupports": [{
            "segment": {"partIndex": 0, "startIndex": 3, "endIndex": 8, "text": "t wor"},
            "groundingChunkIndices": [0]
        }]
    }))?;

    let evidence = accumulator.map_metadata(0, metadata)?;
    let citation = evidence.iter().find_map(|item| match item {
        HostedEvidence::Citation(citation) => Some(citation),
        _ => None,
    });
    assert_eq!(citation.map(|item| item.start_byte()), Some(3));
    assert_eq!(citation.map(|item| item.end_byte()), Some(8));
    Ok(())
}

#[test]
fn exact_model_matrix_fails_closed_for_unknown_or_unsupported_models() {
    for model in [
        "",
        "gemini-test",
        "gemini-pro",
        "gemini-1.5-pro",
        "gemini-2.5-flash-preview-unknown",
        "gemini-3.5-flash-latest",
        "publishers/google/models/gemini-2.5-flash",
    ] {
        assert!(
            !gemini_hosted_web_search_supported(model),
            "expected {model} to fail closed"
        );
    }
}

#[test]
fn exact_model_matrix_limits_custom_tool_combinations_to_gemini_three() {
    assert!(gemini_hosted_custom_tools_supported("gemini-3.5-flash"));
    assert!(gemini_hosted_custom_tools_supported(
        "models/gemini-3.1-pro-preview"
    ));
    assert!(!gemini_hosted_custom_tools_supported("gemini-2.5-pro"));
    assert!(!gemini_hosted_custom_tools_supported("gemini-unknown"));
}

#[test]
fn grounding_wire_dto_redacts_raw_query_url_title_and_segment_text() -> anyhow::Result<()> {
    let metadata: GeminiGroundingMetadata = serde_json::from_value(serde_json::json!({
        "webSearchQueries": ["raw private query"],
        "groundingChunks": [{
            "web": {"uri": "https://example.com/?token=raw", "title": "raw title"}
        }],
        "groundingSupports": [{
            "segment": {"partIndex": 0, "startIndex": 0, "endIndex": 3, "text": "raw"},
            "groundingChunkIndices": [0]
        }]
    }))?;

    let debug = format!("{metadata:?}");
    assert!(!debug.contains("raw private query"));
    assert!(!debug.contains("token=raw"));
    assert!(!debug.contains("raw title"));
    Ok(())
}
