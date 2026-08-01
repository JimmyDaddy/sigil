use sigil_kernel::{
    DisclosurePresentationError, DisclosurePresentationReceipt, EgressDisclosurePresenter,
    ExternalEvidenceLevel, ExternalSourceRecord, RootConfig, SourceCacheStatus, SourceFreshness,
    ToolAnalysisStatus, ToolCall, ToolContext, ToolOperation, ToolPermissionEffect, ToolRegistry,
    ToolRestartPolicy,
};

use crate::ProxyEnvironment;

use super::*;

struct AcceptingPresenter;

fn current_root(extra: &str) -> RootConfig {
    toml::from_str(&format!(
        r#"config_version = 2

[agent]
connection = "local"
model = "test"

[connections.local]
label = "Local"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:11434/v1"
credential = {{ source = "none" }}

{extra}
"#
    ))
    .expect("current root config should parse")
}

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
    let config = current_root("");
    let mut registry = ToolRegistry::new();
    register_web_search_tool(&mut registry, &config, 64, Arc::new(AcceptingPresenter));
    let spec = registry
        .spec_for("websearch")
        .expect("default Web V1 should expose websearch");
    assert!(spec.description.contains("used directly"));
    assert!(spec.description.contains("Do not automatically fan out"));
    assert!(spec.description.contains("explicitly asks"));
}

#[test]
fn websearch_permission_plan_declares_only_network_read_and_keeps_query_out_of_scope() {
    let config = current_root("");
    let mut registry = ToolRegistry::new();
    register_web_search_tool(&mut registry, &config, 64, Arc::new(AcceptingPresenter));
    let context = ToolContext::new(std::env::current_dir().expect("current dir"), 5);
    let plan = |id: &str, query: &str| {
        registry
            .permission_plan(
                &context,
                &ToolCall {
                    id: id.to_owned(),
                    name: "websearch".to_owned(),
                    args_json: serde_json::json!({ "query": query, "max_results": 5 }).to_string(),
                },
            )
            .expect("websearch plan should resolve")
    };
    let first = plan("search-1", "structured permission plans");
    let second = plan("search-2", "event driven terminal lifecycle");

    assert_eq!(first.operation, ToolOperation::NetworkRequest);
    assert_eq!(first.analysis, ToolAnalysisStatus::Complete);
    assert_eq!(
        first.effects,
        std::collections::BTreeSet::from([ToolPermissionEffect::NetworkRead])
    );
    assert_eq!(first.semantic_scope, second.semantic_scope);
    assert_ne!(first.plan_hash, second.plan_hash);
    assert_eq!(
        first.analysis_bindings.get("planner").map(String::as_str),
        Some("websearch_v2")
    );
    assert!(
        !first
            .safe_summary
            .detail
            .contains("structured permission plans")
    );
}

#[test]
fn configured_websearch_query_disclosure_uses_the_remote_mcp_origin() {
    let config = current_root(
        r#"[web]
proxy_mode = "direct"
search_route = "mcp"

[web.search_mcp]
server = "search"
tool = "search"

[[mcp_servers]]
name = "search"
transport = "streamable_http"
url = "https://search.example.test/mcp"
startup = "lazy"
"#,
    );
    let binding = config.web.search_mcp.as_ref().expect("configured binding");

    let destination = configured_query_egress_destination(&config, binding)
        .expect("configured remote MCP should have a safe query destination");

    assert_eq!(
        destination.safe_logical_destination,
        "https://search.example.test/"
    );
    assert_eq!(
        destination.safe_transport_destination,
        "https://search.example.test/"
    );
    assert_eq!(destination.route, EgressNetworkRoute::Direct);

    let mut registry = ToolRegistry::new();
    register_web_search_tool(&mut registry, &config, 64, Arc::new(AcceptingPresenter));
    let call = ToolCall {
        id: "configured-search-plan".to_owned(),
        name: "websearch".to_owned(),
        args_json: json!({ "query": "runtime permission plan" }).to_string(),
    };
    let context = ToolContext::new(std::env::current_dir().expect("current dir"), 5);
    let plan = registry
        .permission_plan(&context, &call)
        .expect("configured search plan");
    assert_eq!(
        plan.tool_default_mode,
        Some(sigil_kernel::ApprovalMode::Ask)
    );
    assert!(
        plan.subjects
            .iter()
            .any(|subject| { subject.normalized.starts_with("mcp__search__search") })
    );
    assert!(
        plan.subjects
            .iter()
            .any(|subject| subject.normalized == "mcp_trust_class:self_hosted")
    );
    assert_eq!(
        plan.semantic_scope
            .as_ref()
            .and_then(|scope| scope.qualifiers.get("route"))
            .map(String::as_str),
        Some("configured_mcp")
    );
}

#[test]
fn bundled_websearch_query_disclosure_uses_the_environment_proxy_route() {
    let config = current_root("");

    let destination = query_egress_destination_with_proxy(
        &config,
        Url::parse(BUNDLED_SEARCH_ENDPOINT).expect("bundled endpoint"),
        ProxyEnvironment::from_values(
            None,
            Some(SecretString::new("http://proxy.example.test:8080")),
            None,
            None,
        ),
    )
    .expect("bundled route should use the configured proxy");

    assert_eq!(destination.safe_logical_destination, "https://mcp.exa.ai/");
    assert_eq!(
        destination.safe_transport_destination,
        "http://proxy.example.test:8080/"
    );
    assert_eq!(destination.route, EgressNetworkRoute::ProxyRemote);
}
