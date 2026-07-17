use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use sigil_kernel::{
    DisclosurePresentationError, DisclosurePresentationReceipt, DurableEventType,
    EgressDataCategory, EgressDisclosureKind, EgressDisclosurePresenter, EgressNetworkRoute,
    JsonlSessionStore, Session, SessionStreamRecord, WebBudgetReservationKind,
    WebBudgetReservationRequest, WebQueryEgressClass, WebTaskTreeBudget, WebTaskTreeBudgetLimits,
};
use tempfile::tempdir;

use super::*;

#[test]
fn oauth_challenge_remains_a_non_retrying_connector_failure_until_runtime_auth_is_wired() {
    let challenge = sigil_mcp::McpOAuthChallenge::parse(
        "Bearer",
        SecretString::new("https://resource.example/mcp"),
    )
    .expect("valid challenge")
    .expect("Bearer challenge");
    let failure = mcp_failure(&McpStreamableHttpError::OAuthRequired(Box::new(challenge)));

    assert_eq!(failure.class, WebSearchFailureClass::OAuthUnsupported);
}

struct ReceiptPresenter;

#[async_trait]
impl EgressDisclosurePresenter for ReceiptPresenter {
    async fn present(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        disclosure.presentation_receipt("stable-search-test-sink")
    }
}

struct OneAttemptFactory {
    attempt: Mutex<Option<RuntimeStableSearchQueryAttempt>>,
}

#[async_trait]
impl StableSearchQueryAttemptFactory for OneAttemptFactory {
    async fn next_attempt(
        &self,
        _request: &WebSearchRequest,
        _identity: &WebSearchConnectorIdentity,
    ) -> Result<RuntimeStableSearchQueryAttempt, WebSearchConnectorError> {
        self.attempt
            .lock()
            .expect("attempt lock")
            .take()
            .ok_or_else(|| failed(WebSearchFailureClass::UnexpectedResponse))
    }
}

#[derive(Clone, Copy)]
enum FakeOutcome {
    Success,
    GenericMixed,
    PreBodyTransport,
    RateLimited,
}

struct FakeTransport {
    tool: McpRemoteTool,
    outcome: FakeOutcome,
    calls: AtomicUsize,
    arguments: Mutex<Vec<Value>>,
    live_header_fingerprint: String,
    profile_fingerprint: String,
}

#[async_trait]
impl StableMcpSearchTransport for FakeTransport {
    fn auth_state(&self) -> McpStreamableHttpAuthState {
        McpStreamableHttpAuthState::Anonymous
    }

    fn transport_fingerprint(&self) -> String {
        self.profile_fingerprint.clone()
    }

    fn live_header_fingerprint(&self) -> String {
        self.live_header_fingerprint.clone()
    }

    fn profile_config_proxy_fingerprint(&self) -> String {
        self.profile_fingerprint.clone()
    }

    async fn server_identity(&self) -> Option<McpRemoteServerIdentity> {
        Some(McpRemoteServerIdentity {
            name: BUNDLED_SERVER_NAME.to_owned(),
            version: BUNDLED_SERVER_VERSION.to_owned(),
            fingerprint: bundled_server_identity_fingerprint(),
        })
    }

    async fn list_tools(&self) -> Result<Vec<McpRemoteTool>, McpStreamableHttpError> {
        Ok(vec![self.tool.clone()])
    }

    async fn call_tool(
        &self,
        _tool: &McpRemoteTool,
        arguments: Value,
        _cancellation: Option<&RunCancellationHandle>,
        observer: Arc<dyn McpRequestBodyObserver>,
    ) -> Result<McpCallToolResult, McpStreamableHttpError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.arguments.lock().expect("arguments").push(arguments);
        if matches!(self.outcome, FakeOutcome::PreBodyTransport) {
            return Err(McpStreamableHttpError::Transport);
        }
        observer.on_first_body_poll()?;
        if matches!(self.outcome, FakeOutcome::RateLimited) {
            return Err(McpStreamableHttpError::RateLimited);
        }
        if matches!(self.outcome, FakeOutcome::GenericMixed) {
            return Ok(McpCallToolResult {
                content: vec![
                    json!({"type":"text","text":"first"}),
                    json!({"type":"image","data":"ignored","mimeType":"image/png"}),
                    json!({"type":"text","text":"second known-secret"}),
                ],
                structured_content: Some(json!({"url":"https://example.test/?token=raw"})),
                is_error: false,
            });
        }
        Ok(McpCallToolResult {
            content: vec![json!({
                "type": "text",
                "text": "Title: Rust\nURL: https://example.test/rust\nPublished: 2026-07-10T00:00:00Z\nAuthor: Example\nHighlights:\nMemory safety"
            })],
            structured_content: None,
            is_error: false,
        })
    }
}

struct FakeTransportFactory {
    connects: AtomicUsize,
    transport: Arc<FakeTransport>,
}

#[async_trait]
impl StableMcpSearchTransportFactory for FakeTransportFactory {
    async fn connect(&self) -> Result<Arc<dyn StableMcpSearchTransport>, McpStreamableHttpError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::clone(&self.transport) as Arc<dyn StableMcpSearchTransport>)
    }
}

fn request() -> WebSearchRequest {
    WebSearchRequest {
        correlation_id: "query-correlation".to_owned(),
        query: SecretString::new("rust language"),
        query_chars: 13,
        query_bytes: 13,
        provenance: WebQueryEgressClass::UserProvided,
        max_results: 3,
        retrieved_at: "2026-07-11T10:00:00Z".to_owned(),
        cancellation: None,
    }
}

fn fixture(
    outcome: FakeOutcome,
    schema: Value,
) -> (
    tempfile::TempDir,
    JsonlSessionStore,
    Arc<WebTaskTreeBudget>,
    Arc<FakeTransportFactory>,
    BundledExaSearchConnector,
) {
    let temp = tempdir().expect("temp");
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl")).expect("store");
    let recorder = Session::new("provider", "model")
        .with_store(store.clone())
        .egress_audit_recorder()
        .expect("recorder");
    let budget = WebTaskTreeBudget::new(
        "root-run",
        WebTaskTreeBudgetLimits {
            max_fetch_calls: 4,
            max_client_search_calls: 4,
            max_hosted_requests: 1,
            max_network_attempts: 4,
            max_wire_bytes: 1024 * 1024,
            max_decoded_bytes: 1024 * 1024,
            max_model_bytes: 1024 * 1024,
            max_concurrent_requests: 2,
            max_attempts_per_host: 4,
        },
        None,
    )
    .expect("budget");
    let reservation = budget
        .reserve(WebBudgetReservationRequest {
            correlation_id: "query-correlation".to_owned(),
            attempt_id: "query-attempt".to_owned(),
            route_lease_id: "query-lease".to_owned(),
            route_fingerprint: "query-route".to_owned(),
            kind: WebBudgetReservationKind::ClientSearchCall,
        })
        .expect("reservation");
    let disclosure = PreEgressDisclosure::new(
        EgressDisclosureKind::Query,
        Some("query-correlation".to_owned()),
        "query-disclosure",
        "tui",
        "Anonymous Exa web search",
        "query-route",
        bundled_profile_fingerprint(),
        "https://mcp.exa.ai/",
        "https://mcp.exa.ai/",
        EgressNetworkRoute::Direct,
        vec![EgressDataCategory::SearchQuery],
    )
    .expect("disclosure");
    let started = QueryEgressStarted {
        record_id: "query-start".to_owned(),
        root_run_id: "root-run".to_owned(),
        correlation_id: "query-correlation".to_owned(),
        route_lease_id: "query-lease".to_owned(),
        route_fingerprint: "query-route".to_owned(),
        query_chars: 13,
        query_bytes: 13,
        egress_class: WebQueryEgressClass::UserProvided,
    };
    let attempt_factory = Arc::new(OneAttemptFactory {
        attempt: Mutex::new(Some(RuntimeStableSearchQueryAttempt {
            disclosure,
            started,
            reservation,
        })),
    });
    let permits = Arc::new(RuntimeStableSearchQueryPermitFactory::new(
        EgressOrderingCoordinator::new(recorder, Some(Arc::new(ReceiptPresenter))),
        attempt_factory,
        Arc::new(|| true),
    ));
    let transport = Arc::new(FakeTransport {
        tool: McpRemoteTool {
            name: BUNDLED_TOOL_NAME.to_owned(),
            description: None,
            input_schema: schema,
            output_schema: None,
            task_support: Some("forbidden".to_owned()),
        },
        outcome,
        calls: AtomicUsize::new(0),
        arguments: Mutex::new(Vec::new()),
        live_header_fingerprint: format!("hmac-sha256:{}", sha256("test-live-headers")),
        profile_fingerprint: bundled_profile_fingerprint(),
    });
    let transports = Arc::new(FakeTransportFactory {
        connects: AtomicUsize::new(0),
        transport,
    });
    let connector = BundledExaSearchConnector {
        transports: Arc::clone(&transports) as Arc<dyn StableMcpSearchTransportFactory>,
        transport: OnceCell::new(),
        permits,
        redactor: SecretRedactor::empty(),
        session_scope_id: "session-test".to_owned(),
    };
    (temp, store, budget, transports, connector)
}

fn event_count(store: &JsonlSessionStore, kind: DurableEventType) -> usize {
    store
        .read_event_records_writer()
        .expect("records")
        .into_iter()
        .filter(|record| {
            matches!(record, SessionStreamRecord::Stored(event) if event.event_kind() == Some(kind))
        })
        .count()
}

#[tokio::test]
async fn bundled_route_is_lazy_and_emits_only_pinned_arguments_and_snippets() {
    let (_temp, store, budget, factory, connector) =
        fixture(FakeOutcome::Success, bundled_input_schema());
    assert_eq!(factory.connects.load(Ordering::SeqCst), 0);

    let response = connector.search(request()).await.expect("search");

    assert_eq!(factory.connects.load(Ordering::SeqCst), 1);
    assert_eq!(factory.transport.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        factory
            .transport
            .arguments
            .lock()
            .expect("arguments")
            .as_slice(),
        &[json!({"query": "rust language", "numResults": 3})]
    );
    assert_eq!(response.sources.len(), 1);
    assert_eq!(budget.snapshot().expect("budget").logical_calls, 1);
    assert_eq!(event_count(&store, DurableEventType::QueryEgressStarted), 1);
    assert_eq!(event_count(&store, DurableEventType::QueryEgressOutcome), 1);
}

#[tokio::test]
async fn schema_drift_fails_before_query_authorization_or_body_bytes() {
    let mut schema = bundled_input_schema();
    schema["properties"]["extra"] = json!({"type": "string"});
    let (_temp, store, budget, factory, connector) = fixture(FakeOutcome::Success, schema);

    let error = connector.search(request()).await.expect_err("schema drift");

    assert!(matches!(
        error,
        WebSearchConnectorError::Failed(WebSearchFailure {
            class: WebSearchFailureClass::SchemaDrift,
            ..
        })
    ));
    assert_eq!(factory.transport.calls.load(Ordering::SeqCst), 0);
    assert_eq!(budget.snapshot().expect("budget").logical_calls, 0);
    assert_eq!(event_count(&store, DurableEventType::QueryEgressStarted), 0);
}

#[tokio::test]
async fn pre_body_transport_failure_gets_one_terminal_without_logical_charge() {
    let (_temp, store, budget, factory, connector) =
        fixture(FakeOutcome::PreBodyTransport, bundled_input_schema());

    let error = connector.search(request()).await.expect_err("transport");

    assert!(matches!(
        error,
        WebSearchConnectorError::Failed(WebSearchFailure {
            class: WebSearchFailureClass::TransportUnavailable,
            ..
        })
    ));
    assert_eq!(factory.transport.calls.load(Ordering::SeqCst), 1);
    assert_eq!(budget.snapshot().expect("budget").logical_calls, 0);
    assert_eq!(event_count(&store, DurableEventType::QueryEgressOutcome), 1);
}

#[tokio::test]
async fn post_start_rate_limit_is_not_retried_and_has_one_terminal() {
    let (_temp, store, budget, factory, connector) =
        fixture(FakeOutcome::RateLimited, bundled_input_schema());

    let error = connector.search(request()).await.expect_err("rate limit");

    assert!(matches!(
        error,
        WebSearchConnectorError::Failed(WebSearchFailure {
            class: WebSearchFailureClass::RateLimited,
            ..
        })
    ));
    assert_eq!(factory.transport.calls.load(Ordering::SeqCst), 1);
    assert_eq!(budget.snapshot().expect("budget").logical_calls, 1);
    assert_eq!(event_count(&store, DurableEventType::QueryEgressOutcome), 1);
}

#[tokio::test]
async fn configured_generic_adapter_sends_only_query_and_never_projects_sources() {
    let generic_schema = json!({
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "query"},
            "optional": {"type": "integer"}
        },
        "required": ["query"],
        "additionalProperties": false
    });
    let (_temp, _store, _budget, factory, bundled) =
        fixture(FakeOutcome::GenericMixed, generic_schema.clone());
    let tool_fingerprint = mcp_tool_schema_fingerprint(&factory.transport.tool);
    let registry = Arc::new(McpSearchBindingRegistry::default());
    let revision = registry
        .declare(crate::PendingMcpSearchBinding {
            server_name: "configured-search".to_owned(),
            tool_name: BUNDLED_TOOL_NAME.to_owned(),
            origin: crate::McpSearchBindingOrigin::UserConfigured,
            root_run_id: "root-run".to_owned(),
            config_epoch: 1,
        })
        .expect("declare");
    registry
        .activate(
            revision,
            Ok(crate::PreparedMcpSearchBinding {
                server_name: "configured-search".to_owned(),
                tool_name: BUNDLED_TOOL_NAME.to_owned(),
                origin: crate::McpSearchBindingOrigin::UserConfigured,
                adapter: McpSearchAdapterKind::GenericQueryText,
                safe_destination: "https://mcp.exa.ai/".to_owned(),
                server_identity_fingerprint: bundled_server_identity_fingerprint(),
                tool_schema_fingerprint: tool_fingerprint,
                transport_fingerprint: factory.transport.profile_fingerprint.clone(),
                live_header_fingerprint: factory.transport.live_header_fingerprint.clone(),
                source_policy_fingerprint: sha256("source-policy"),
                effective_policy_fingerprint: sha256("effective-policy"),
                profile_config_proxy_fingerprint: factory.transport.profile_fingerprint.clone(),
                root_run_id: "root-run".to_owned(),
                config_epoch: 1,
            }),
        )
        .expect("activate");
    let crate::StableMcpRouteSelection::Configured(lease) =
        registry.select_auto(true).expect("route")
    else {
        panic!("configured route expected");
    };
    let connector = ConfiguredStableMcpSearchConnector {
        registry,
        lease: *lease,
        transport: Arc::clone(&factory.transport) as Arc<dyn StableMcpSearchTransport>,
        permits: Arc::clone(&bundled.permits),
        redactor: SecretRedactor::from_values(["known-secret"]),
        session_scope_id: "session-test".to_owned(),
    };

    let response = connector.search(request()).await.expect("generic search");

    assert!(response.sources.is_empty());
    assert!(response.safe_model_content.contains("first\n\nsecond"));
    assert!(!response.safe_model_content.contains("known-secret"));
    assert!(!response.safe_model_content.contains("token=raw"));
    assert_eq!(
        response.source_projection,
        SourceProjection::Unavailable {
            reason: SourceProjectionUnavailableReason::GenericAdapterNoSourceContract,
        }
    );
    assert_eq!(
        factory
            .transport
            .arguments
            .lock()
            .expect("arguments")
            .as_slice(),
        &[json!({"query": "rust language"})]
    );
}
