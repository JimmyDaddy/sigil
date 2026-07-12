use sigil_kernel::{
    DisclosurePresentationError, DisclosurePresentationReceipt, EgressDisclosurePresenter,
    ExternalEvidenceLevel, ExternalSourceRecord, RootConfig, SourceCacheStatus, SourceFreshness,
    ToolRegistry, ToolRestartPolicy,
};

use super::*;

struct AcceptingPresenter;

#[async_trait::async_trait]
impl EgressDisclosurePresenter for AcceptingPresenter {
    async fn present(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        disclosure.presentation_receipt("websearch-public-tool-test")
    }
}

fn response(session_scope_id: &str) -> crate::WebSearchResponse {
    let source = ExternalSourceRecord::from_remote_candidate(
        session_scope_id,
        None,
        ExternalEvidenceLevel::SearchSnippet,
        "https://example.com/result",
        "exa_mcp",
        Some("Example result".to_owned()),
        None,
        "2026-07-12T00:00:00Z",
        None,
        Some(0),
        SourceFreshness::Unknown,
        SourceCacheStatus::NotApplicable,
        ToolRestartPolicy::Replayable,
    )
    .expect("source should be valid");
    crate::WebSearchResponse {
        safe_model_content: "Title: Example result\nURL: https://example.com/result".to_owned(),
        source_projection: crate::SourceProjection::Structured {
            codec_id: "exa_text_v1".to_owned(),
            valid_records: 1,
        },
        source_capabilities: vec![crate::WebSearchSourceCapability {
            source_id: source.source_id.clone(),
            raw_canonical_url: SecretString::new("https://example.com/result"),
            safe_display_url: source.safe_display_url.clone(),
            restart_policy: ToolRestartPolicy::Replayable,
        }],
        sources: vec![source],
    }
}

#[test]
fn bundled_search_result_requires_the_active_session_scope() {
    let result = search_result(
        "call-search".to_owned(),
        response("session-active"),
        "session-active",
    )
    .expect("matching scope should produce a tool result");
    assert_eq!(result.external_sources.len(), 1);
    assert_eq!(
        result.external_sources[0].session_scope_id,
        "session-active"
    );

    let error = search_result(
        "call-search".to_owned(),
        response("root-run-id"),
        "session-active",
    )
    .expect_err("run identity must not be accepted as a session scope");
    assert!(
        error
            .to_string()
            .contains("does not belong to the active session scope")
    );
}

#[test]
fn public_websearch_description_discourages_unnecessary_fetch_fanout() {
    let config: RootConfig = toml::from_str(
        r#"[agent]
provider = "deepseek"
model = "test"
"#,
    )
    .expect("root config should parse");
    let mut registry = ToolRegistry::new();
    register_web_search_tool(&mut registry, &config, 64, Arc::new(AcceptingPresenter));
    let spec = registry
        .spec_for("websearch")
        .expect("default Web V1 should expose websearch");
    assert!(spec.description.contains("used directly"));
    assert!(spec.description.contains("Do not automatically fan out"));
    assert!(spec.description.contains("explicitly asks"));
}
