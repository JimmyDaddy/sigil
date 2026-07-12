use super::*;

fn decode(raw: &str) -> WebSearchResponse {
    decode_exa_text_v1(
        raw,
        "session-test",
        "2026-07-11T10:00:00Z",
        &SecretRedactor::from_values(["known-secret"]),
    )
}

#[test]
fn valid_records_become_search_snippets_without_claim_citations() {
    let response = decode(
        "Title: First\nURL: https://example.test/a\nPublished: 2026-07-10T09:00:00Z\nAuthor: A\nHighlights:\nUseful highlight\n\n---\n\nTitle: Second\nURL: https://example.test/b\nPublished: \nAuthor: B\nText: Full text",
    );

    assert_eq!(response.sources.len(), 2);
    assert_eq!(response.source_capabilities.len(), 2);
    assert!(
        response
            .sources
            .iter()
            .all(|source| source.evidence_level == ExternalEvidenceLevel::SearchSnippet)
    );
    assert_eq!(
        response.source_projection,
        SourceProjection::Structured {
            codec_id: EXA_TEXT_V1_CODEC_ID.to_owned(),
            valid_records: 2,
        }
    );
}

#[test]
fn malformed_and_duplicate_records_do_not_create_sources() {
    let response = decode(
        "Title: Bad\nURL: javascript:alert(1)\nPublished: \nAuthor: X\nText: nope\n\n---\n\nTitle: Good\nURL: https://example.test/a\nPublished: \nAuthor: A\nText: one\n\n---\n\nTitle: Duplicate\nURL: https://example.test/a\nPublished: \nAuthor: B\nText: two",
    );

    assert_eq!(response.sources.len(), 1);
    assert_eq!(response.source_capabilities.len(), 1);
    assert_eq!(response.sources[0].title.as_deref(), Some("Good"));
}

#[test]
fn format_drift_and_invalid_records_degrade_without_sources() {
    let drift = decode("unstructured answer with known-secret\u{1b}[31m");
    assert!(drift.sources.is_empty());
    assert!(drift.source_capabilities.is_empty());
    assert_eq!(
        drift.source_projection,
        SourceProjection::Unavailable {
            reason: SourceProjectionUnavailableReason::CodecFormatDrift,
        }
    );
    assert!(!drift.safe_model_content.contains("known-secret"));
    assert!(!drift.safe_model_content.contains('\u{1b}'));

    let invalid = decode(
        "Title: Bad\nURL: https://user:pass@example.test/\nPublished: \nAuthor: X\nText: nope",
    );
    assert!(invalid.sources.is_empty());
    assert!(invalid.source_capabilities.is_empty());
    assert_eq!(
        invalid.source_projection,
        SourceProjection::Unavailable {
            reason: SourceProjectionUnavailableReason::NoValidRecords,
        }
    );
}

#[test]
fn query_and_fragment_are_safely_projected_but_raw_material_is_not_retained() {
    let response = decode(
        "Title: Query\nURL: https://example.test/path?q=secret\nPublished: \nAuthor: A\nText: body",
    );
    assert_eq!(response.sources.len(), 1);
    assert_eq!(
        response.sources[0].safe_display_url,
        "https://example.test/path?[redacted]"
    );
    assert!(!response.safe_model_content.contains("q=secret"));
    assert_eq!(response.source_capabilities.len(), 1);
    assert_eq!(
        response.source_capabilities[0]
            .raw_canonical_url
            .expose_secret(),
        "https://example.test/path?q=secret"
    );
    assert!(!format!("{:?}", response.source_capabilities).contains("q=secret"));
}
