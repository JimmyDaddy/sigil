use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, WWW_AUTHENTICATE};
use serde_json::json;
use sigil_kernel::{RunCancellationOwner, SecretString};

use super::*;

#[path = "streamable_http_test_support.rs"]
mod support;
use support::{FixtureResponse, FixtureServer, MapHeaderEnvironment, PlanAuthorizer};

fn initialize_json(id: u64, version: &str, list_changed: bool) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": list_changed } },
            "serverInfo": { "name": "fixture", "version": "1.0" }
        }
    })
    .to_string()
}

async fn connect_fixture(
    responses: Vec<FixtureResponse>,
    capabilities: McpRemoteClientCapabilities,
) -> (Arc<McpStreamableHttpClient>, FixtureServer) {
    let server = FixtureServer::start(responses).await;
    let authorizer = PlanAuthorizer::direct(server.endpoint());
    let client = McpStreamableHttpClient::connect(
        Arc::new(authorizer),
        &McpStreamableHttpHeaderConfig::default(),
        &MapHeaderEnvironment::default(),
        capabilities,
        McpStreamableHttpLimits::default(),
    )
    .await
    .expect("fixture should initialize");
    (client, server)
}

#[tokio::test]
async fn streamable_http_initialization_stages_session_until_202_barrier() {
    let (client, server) = connect_fixture(
        vec![
            FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, true))
                .with_header("Mcp-Session-Id", "session-secret"),
            FixtureResponse::empty(202),
        ],
        McpRemoteClientCapabilities::empty(),
    )
    .await;
    assert_eq!(client.lifecycle().await, McpStreamableHttpLifecycle::Ready);
    assert_eq!(
        client.protocol_version().await,
        Some(McpRemoteProtocolVersion::V2025_11_25)
    );
    let identity = client.server_identity().await.expect("server identity");
    assert_eq!(identity.name, "fixture");
    assert_eq!(identity.version, "1.0");
    assert_eq!(identity.fingerprint.len(), 64);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].to_ascii_lowercase().contains("mcp-session-id"));
    assert!(requests[1].contains("mcp-session-id: session-secret"));
    assert!(requests[1].contains("notifications/initialized"));
    assert!(format!("{client:?}").find("session-secret").is_none());
}

struct CountingBodyObserver(AtomicUsize);

impl McpRequestBodyObserver for CountingBodyObserver {
    fn on_first_body_poll(&self) -> Result<(), McpStreamableHttpError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn streamable_http_body_observer_runs_exactly_once_on_real_first_poll() {
    let (client, server) = connect_fixture(
        vec![
            FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false)),
            FixtureResponse::empty(202),
            FixtureResponse::json(
                200,
                json!({"jsonrpc":"2.0","id":2,"result":{"content":[]}}).to_string(),
            ),
        ],
        McpRemoteClientCapabilities::empty(),
    )
    .await;
    let observer = Arc::new(CountingBodyObserver(AtomicUsize::new(0)));
    client
        .call_tool_with_body_observer(
            &empty_tool("observed"),
            json!({}),
            None,
            &|| true,
            Some(Arc::clone(&observer) as Arc<dyn McpRequestBodyObserver>),
        )
        .await
        .expect("call");
    assert_eq!(observer.0.load(Ordering::SeqCst), 1);
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|request| request.contains("tools/call"))
            .count(),
        1
    );
}

#[tokio::test]
async fn streamable_http_initialized_nonempty_body_is_terminal() {
    let server = FixtureServer::start(vec![
        FixtureResponse::json(200, initialize_json(1, PREVIOUS_PROTOCOL_VERSION, false)),
        FixtureResponse::json(202, "{}"),
    ])
    .await;
    let error = McpStreamableHttpClient::connect(
        Arc::new(PlanAuthorizer::direct(server.endpoint())),
        &McpStreamableHttpHeaderConfig::default(),
        &MapHeaderEnvironment::default(),
        McpRemoteClientCapabilities::empty(),
        McpStreamableHttpLimits::default(),
    )
    .await
    .expect_err("nonempty initialized response must fail");
    assert!(matches!(
        error,
        McpStreamableHttpError::InitializedNotificationRejected
    ));
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn streamable_http_redirect_is_not_followed_or_replayed() {
    let server = FixtureServer::start(vec![
        FixtureResponse::empty(307).with_header("Location", "http://127.0.0.1:9/steal"),
    ])
    .await;
    let error = McpStreamableHttpClient::connect(
        Arc::new(PlanAuthorizer::direct(server.endpoint())),
        &McpStreamableHttpHeaderConfig::default(),
        &MapHeaderEnvironment::default(),
        McpRemoteClientCapabilities::empty(),
        McpStreamableHttpLimits::default(),
    )
    .await
    .expect_err("redirect must be terminal");
    assert!(matches!(
        error,
        McpStreamableHttpError::UnexpectedHttpStatus { status: 307 }
    ));
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn streamable_http_json_and_sse_responses_converge_by_id() {
    let sse = format!(
        "event: message\ndata: {}\n\n",
        json!({"jsonrpc":"2.0","id":2,"result":{"tools":[]}})
    );
    let (client, server) = connect_fixture(
        vec![
            FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false)),
            FixtureResponse::empty(202),
            FixtureResponse::sse(200, sse),
        ],
        McpRemoteClientCapabilities::empty(),
    )
    .await;
    let tools = client.list_tools().await.expect("SSE tools response");
    assert!(tools.is_empty());
    assert_eq!(server.requests().len(), 3);
}

#[tokio::test]
async fn streamable_http_get_405_and_delete_405_are_tolerated() {
    let (client, server) = connect_fixture(
        vec![
            FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false))
                .with_header("Mcp-Session-Id", "live-session"),
            FixtureResponse::empty(202),
            FixtureResponse::empty(405),
            FixtureResponse::empty(405),
        ],
        McpRemoteClientCapabilities::empty(),
    )
    .await;
    assert!(
        !client
            .probe_get_listener()
            .await
            .expect("GET 405 is optional")
    );
    client.close(true).await.expect("DELETE 405 is optional");
    assert_eq!(client.lifecycle().await, McpStreamableHttpLifecycle::Closed);
    assert_eq!(server.requests().len(), 4);
}

#[test]
fn streamable_http_headers_enforce_ownership_tls_and_live_hmac() {
    let environment = MapHeaderEnvironment(BTreeMap::from([
        ("TOKEN".to_owned(), SecretString::new("top-secret")),
        ("TRACE".to_owned(), SecretString::new("trace-value")),
    ]));
    let sensitive = McpStreamableHttpHeaderConfig {
        bearer_token_env_var: Some("TOKEN".to_owned()),
        ..Default::default()
    };
    assert!(matches!(
        auth::resolve_headers(
            &sensitive,
            &environment,
            &Url::parse("http://example.test/mcp").expect("url")
        ),
        Err(McpStreamableHttpError::ConfigurationInvalid)
    ));
    let https = Url::parse("https://example.test/mcp").expect("url");
    let first = auth::resolve_headers(&sensitive, &environment, &https).expect("HTTPS auth");
    let second =
        auth::resolve_headers(&sensitive, &environment, &https).expect("stable process HMAC");
    assert_eq!(first.live_fingerprint, second.live_fingerprint);
    assert!(!format!("{first:?}").contains("top-secret"));
    let owned = McpStreamableHttpHeaderConfig {
        literal: BTreeMap::from([("Cookie".to_owned(), "x=y".to_owned())]),
        ..Default::default()
    };
    assert!(auth::resolve_headers(&owned, &environment, &https).is_err());
}

#[test]
fn streamable_http_401_classification_distinguishes_plain_static_and_oauth() {
    let plain = HeaderMap::new();
    assert!(matches!(
        auth::classify_unauthorized(&plain, false),
        Err(McpStreamableHttpError::AuthenticationRequired)
    ));
    assert!(matches!(
        auth::classify_unauthorized(&plain, true),
        Err(McpStreamableHttpError::AuthenticationFailed)
    ));
    let mut oauth = HeaderMap::new();
    oauth.insert(
        WWW_AUTHENTICATE,
        HeaderValue::from_static(
            "Bearer resource_metadata=\"https://auth.example/.well-known/oauth\"",
        ),
    );
    assert!(matches!(
        auth::classify_unauthorized(&oauth, false),
        Err(McpStreamableHttpError::OAuthUnsupported)
    ));
    let mut invalid = HeaderMap::new();
    invalid.insert(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer resource_metadata=\"http://127.0.0.1/meta\""),
    );
    assert!(matches!(
        auth::classify_unauthorized(&invalid, false),
        Err(McpStreamableHttpError::InvalidAuthenticationChallenge)
    ));
}

#[tokio::test]
async fn streamable_http_cancel_sends_at_most_one_notification_without_replaying_query() {
    let (client, server) = connect_fixture(
        vec![
            FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false))
                .with_header("Mcp-Session-Id", "live-session"),
            FixtureResponse::empty(202),
            FixtureResponse::json(
                200,
                json!({"jsonrpc":"2.0","id":2,"result":{"content":[]}}).to_string(),
            )
            .with_delay(Duration::from_secs(5)),
            FixtureResponse::empty(202),
        ],
        McpRemoteClientCapabilities::empty(),
    )
    .await;
    let tool = McpRemoteTool {
        name: "slow".to_owned(),
        description: None,
        input_schema: json!({"type":"object","properties":{},"additionalProperties":false}),
        output_schema: None,
        task_support: None,
    };
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let client_for_call = Arc::clone(&client);
    let call = tokio::spawn(async move {
        client_for_call
            .call_tool(&tool, json!({}), Some(&handle), &|| true)
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    owner.request_cancel();
    let error = call.await.expect("join").expect_err("cancelled call");
    assert!(matches!(error, McpStreamableHttpError::Cancelled));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let requests = server.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains("tools/call"))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains("notifications/cancelled"))
            .count(),
        1
    );
}

#[tokio::test]
async fn streamable_http_authorizes_every_message_and_uses_fresh_connections() {
    let server = FixtureServer::start(vec![
        FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false)),
        FixtureResponse::empty(202),
        FixtureResponse::json(
            200,
            json!({"jsonrpc":"2.0","id":2,"result":{"tools":[]}}).to_string(),
        ),
    ])
    .await;
    let authorizer = PlanAuthorizer::direct(server.endpoint());
    let calls = authorizer.call_count();
    let client = McpStreamableHttpClient::connect(
        Arc::new(authorizer),
        &McpStreamableHttpHeaderConfig::default(),
        &MapHeaderEnvironment::default(),
        McpRemoteClientCapabilities::empty(),
        McpStreamableHttpLimits::default(),
    )
    .await
    .expect("connect");
    client.list_tools().await.expect("list tools");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(server.requests().len(), 3);
    assert!(
        server
            .requests()
            .iter()
            .all(|request| request.to_ascii_lowercase().contains("connection: close"))
    );
}

#[tokio::test]
async fn streamable_http_session_404_reinitializes_only_on_next_explicit_call() {
    let (client, server) = connect_fixture(
        vec![
            FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false))
                .with_header("Mcp-Session-Id", "old-session"),
            FixtureResponse::empty(202),
            FixtureResponse::empty(404),
            FixtureResponse::json(200, initialize_json(3, LATEST_PROTOCOL_VERSION, false))
                .with_header("Mcp-Session-Id", "new-session"),
            FixtureResponse::empty(202),
            FixtureResponse::json(
                200,
                json!({"jsonrpc":"2.0","id":4,"result":{"tools":[]}}).to_string(),
            ),
        ],
        McpRemoteClientCapabilities::empty(),
    )
    .await;
    assert!(matches!(
        client.list_tools().await,
        Err(McpStreamableHttpError::SessionExpired)
    ));
    assert_eq!(
        client.lifecycle().await,
        McpStreamableHttpLifecycle::Disconnected
    );
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|request| request.contains("tools/list"))
            .count(),
        1
    );
    client.list_tools().await.expect("later call reinitializes");
    let requests = server.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains("tools/list"))
            .count(),
        2
    );
    assert!(requests[4].contains("mcp-session-id: new-session"));
}

#[tokio::test]
async fn streamable_http_pagination_is_bounded_and_list_changed_is_coalesced() {
    let tool = json!({
        "name":"search",
        "inputSchema":{"type":"object","properties":{},"additionalProperties":false}
    });
    let (client, server) = connect_fixture(
        vec![
            FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, true)),
            FixtureResponse::empty(202),
            FixtureResponse::json(
                200,
                json!({"jsonrpc":"2.0","id":2,"result":{"tools":[tool],"nextCursor":"opaque-1"}})
                    .to_string(),
            ),
            FixtureResponse::json(
                200,
                json!({"jsonrpc":"2.0","id":3,"result":{"tools":[]}}).to_string(),
            ),
        ],
        McpRemoteClientCapabilities::empty(),
    )
    .await;
    assert_eq!(client.list_tools().await.expect("pages").len(), 1);
    assert!(server.requests()[3].contains("opaque-1"));
    assert!(client.note_tools_list_changed().await.expect("first"));
    assert!(!client.note_tools_list_changed().await.expect("coalesced"));
    assert!(client.take_tools_list_changed().await);
    assert!(!client.take_tools_list_changed().await);
    assert!(
        !client
            .note_tools_list_changed()
            .await
            .expect("debounced after take")
    );
}

#[tokio::test]
async fn streamable_http_pagination_rejects_empty_repeated_page_and_tool_caps() {
    for pages in [
        vec![json!({"tools":[],"nextCursor":""})],
        vec![
            json!({"tools":[],"nextCursor":"same"}),
            json!({"tools":[],"nextCursor":"same"}),
        ],
    ] {
        let mut responses = vec![
            FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false)),
            FixtureResponse::empty(202),
        ];
        responses.extend(pages.into_iter().enumerate().map(|(index, result)| {
            FixtureResponse::json(
                200,
                json!({"jsonrpc":"2.0","id":index as u64 + 2,"result":result}).to_string(),
            )
        }));
        let (client, _) = connect_fixture(responses, McpRemoteClientCapabilities::empty()).await;
        assert!(matches!(
            client.list_tools().await,
            Err(McpStreamableHttpError::InvalidPagination)
        ));
    }

    let tool = json!({
        "name":"search",
        "inputSchema":{"type":"object","properties":{},"additionalProperties":false}
    });
    let (client, _) = connect_fixture(
        vec![
            FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false)),
            FixtureResponse::empty(202),
            FixtureResponse::json(
                200,
                json!({"jsonrpc":"2.0","id":2,"result":{"tools":vec![tool; MAX_TOOLS + 1]}})
                    .to_string(),
            ),
        ],
        McpRemoteClientCapabilities::empty(),
    )
    .await;
    assert!(matches!(
        client.list_tools().await,
        Err(McpStreamableHttpError::InvalidPagination)
    ));
}

#[tokio::test]
async fn streamable_http_pagination_rejects_more_than_max_pages() {
    let mut responses = vec![
        FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false)),
        FixtureResponse::empty(202),
    ];
    for page in 0..MAX_PAGES {
        responses.push(FixtureResponse::json(
            200,
            json!({
                "jsonrpc":"2.0",
                "id":page as u64 + 2,
                "result":{"tools":[],"nextCursor":format!("cursor-{page}")}
            })
            .to_string(),
        ));
    }
    let (client, server) = connect_fixture(responses, McpRemoteClientCapabilities::empty()).await;
    assert!(matches!(
        client.list_tools().await,
        Err(McpStreamableHttpError::InvalidPagination)
    ));
    assert_eq!(server.requests().len(), MAX_PAGES + 2);
}

#[test]
fn streamable_http_sse_caps_eof_and_id_convergence_are_strict() {
    let limits = McpStreamableHttpLimits::default();
    let eof = format!("data: {}", json!({"jsonrpc":"2.0","id":7,"result":{}}));
    let (response, inbound) =
        framing::parse_sse_response(eof.as_bytes(), 7, limits).expect("EOF event");
    assert_eq!(response["id"], 7);
    assert!(inbound.is_empty());
    let duplicate = format!(
        "data: {0}\n\ndata: {0}\n\n",
        json!({"jsonrpc":"2.0","id":7,"result":{}})
    );
    assert!(matches!(
        framing::parse_sse_response(duplicate.as_bytes(), 7, limits),
        Err(McpStreamableHttpError::ResponseIdMismatch)
    ));
    let mismatch = format!("data: {}\n\n", json!({"jsonrpc":"2.0","id":8,"result":{}}));
    assert!(matches!(
        framing::parse_sse_response(mismatch.as_bytes(), 7, limits),
        Err(McpStreamableHttpError::ResponseIdMismatch)
    ));
    let tiny = McpStreamableHttpLimits {
        max_sse_line_bytes: 8,
        ..limits
    };
    assert!(matches!(
        framing::parse_sse_response(eof.as_bytes(), 7, tiny),
        Err(McpStreamableHttpError::SseLimitExceeded)
    ));
}

#[tokio::test]
async fn streamable_http_content_type_and_body_caps_fail_closed() {
    for content_type in ["application/jsonp", "text/html"] {
        let server = FixtureServer::start(vec![FixtureResponse::body(
            200,
            Some(content_type),
            initialize_json(1, LATEST_PROTOCOL_VERSION, false),
        )])
        .await;
        let error = McpStreamableHttpClient::connect(
            Arc::new(PlanAuthorizer::direct(server.endpoint())),
            &McpStreamableHttpHeaderConfig::default(),
            &MapHeaderEnvironment::default(),
            McpRemoteClientCapabilities::empty(),
            McpStreamableHttpLimits::default(),
        )
        .await
        .expect_err("invalid content type");
        assert!(matches!(
            error,
            McpStreamableHttpError::UnexpectedContentType
        ));
    }
    let (client, _) = connect_fixture(
        vec![
            FixtureResponse::body(
                200,
                Some("Application/Json; charset=utf-8"),
                initialize_json(1, LATEST_PROTOCOL_VERSION, false),
            ),
            FixtureResponse::empty(202),
        ],
        McpRemoteClientCapabilities::empty(),
    )
    .await;
    assert_eq!(client.lifecycle().await, McpStreamableHttpLifecycle::Ready);

    let server = FixtureServer::start(vec![FixtureResponse::json(
        200,
        initialize_json(1, LATEST_PROTOCOL_VERSION, false),
    )])
    .await;
    let limits = McpStreamableHttpLimits {
        max_body_bytes: 16,
        ..McpStreamableHttpLimits::default()
    };
    let error = McpStreamableHttpClient::connect(
        Arc::new(PlanAuthorizer::direct(server.endpoint())),
        &McpStreamableHttpHeaderConfig::default(),
        &MapHeaderEnvironment::default(),
        McpRemoteClientCapabilities::empty(),
        limits,
    )
    .await
    .expect_err("body cap");
    assert!(matches!(error, McpStreamableHttpError::BodyLimitExceeded));
}

#[tokio::test]
async fn streamable_http_response_header_caps_and_duplicate_critical_headers_fail_closed() {
    let oversized = FixtureServer::start(vec![
        FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false))
            .with_header("X-Padding", "x".repeat(1024)),
    ])
    .await;
    let error = McpStreamableHttpClient::connect(
        Arc::new(PlanAuthorizer::direct(oversized.endpoint())),
        &McpStreamableHttpHeaderConfig::default(),
        &MapHeaderEnvironment::default(),
        McpRemoteClientCapabilities::empty(),
        McpStreamableHttpLimits {
            max_header_bytes: 256,
            ..McpStreamableHttpLimits::default()
        },
    )
    .await
    .expect_err("oversized response headers");
    assert!(matches!(error, McpStreamableHttpError::HeaderLimitExceeded));

    for (name, value, transport_may_reject) in [
        ("Content-Type", "application/json", false),
        ("Content-Length", "1", true),
    ] {
        let server = FixtureServer::start(vec![
            FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false))
                .with_header(name, value),
        ])
        .await;
        let error = McpStreamableHttpClient::connect(
            Arc::new(PlanAuthorizer::direct(server.endpoint())),
            &McpStreamableHttpHeaderConfig::default(),
            &MapHeaderEnvironment::default(),
            McpRemoteClientCapabilities::empty(),
            McpStreamableHttpLimits::default(),
        )
        .await
        .expect_err("duplicate critical header");
        assert!(
            matches!(error, McpStreamableHttpError::HeaderLimitExceeded)
                || transport_may_reject && matches!(error, McpStreamableHttpError::Transport)
        );
    }
}

struct CapturingFormHandler {
    calls: Arc<AtomicUsize>,
    messages: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl McpRemoteFormHandler for CapturingFormHandler {
    async fn handle_form(
        &self,
        request: ValidatedMcpFormRequest,
    ) -> Result<McpRemoteFormResponse, McpStreamableHttpError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.messages
            .lock()
            .expect("messages")
            .push(request.safe_message);
        Ok(McpRemoteFormResponse::Accept(json!({"choice":"docs"})))
    }
}

#[tokio::test]
async fn streamable_http_get_sse_dispatches_real_ping_roots_and_validated_form() {
    let sse = [
        json!({"jsonrpc":"2.0","id":"ping-1","method":"ping","params":{}}),
        json!({"jsonrpc":"2.0","id":"roots-1","method":"roots/list","params":{}}),
        json!({
            "jsonrpc":"2.0","id":"form-1","method":"elicitation/create",
            "params":{
                "mode":"form",
                "message":"\u{001b}[31mChoose\u{001b}[0m https://unsafe.example/callback",
                "requestedSchema":{
                    "type":"object",
                    "properties":{"choice":{"type":"string","enum":["docs","code"]}},
                    "required":["choice"],"additionalProperties":false
                }
            }
        }),
    ]
    .into_iter()
    .map(|message| format!("data: {message}\n\n"))
    .collect::<String>();
    let server = FixtureServer::start(vec![
        FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false))
            .with_header("Mcp-Session-Id", "live-session"),
        FixtureResponse::empty(202),
        FixtureResponse::sse(200, sse),
        FixtureResponse::empty(202),
        FixtureResponse::empty(202),
        FixtureResponse::empty(202),
    ])
    .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let messages = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(CapturingFormHandler {
        calls: Arc::clone(&calls),
        messages: Arc::clone(&messages),
    });
    let client = McpStreamableHttpClient::connect_with_inbound(
        Arc::new(PlanAuthorizer::direct(server.endpoint())),
        &McpStreamableHttpHeaderConfig::default(),
        &MapHeaderEnvironment::default(),
        McpRemoteClientCapabilities {
            roots: true,
            form_elicitation: true,
        },
        McpStreamableHttpLimits::default(),
        vec![McpRemoteRoot::new("file:///workspace", "workspace").expect("root")],
        Some(handler),
    )
    .await
    .expect("connect");
    assert!(client.probe_get_listener().await.expect("GET listener"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        messages.lock().expect("messages")[0],
        "Choose [url omitted]"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 6);
    assert!(
        requests
            .iter()
            .any(|request| request.contains("file:///workspace"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.contains("\"action\":\"accept\""))
    );
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("unsafe.example"))
    );
}

#[tokio::test]
async fn streamable_http_initialize_sse_ping_uses_staged_session_without_early_ready() {
    let sse = format!(
        "data: {}\n\ndata: {}\n\n",
        json!({"jsonrpc":"2.0","id":"ping-init","method":"ping","params":{}}),
        serde_json::from_str::<Value>(&initialize_json(1, LATEST_PROTOCOL_VERSION, false))
            .expect("initialize")
    );
    let server = FixtureServer::start(vec![
        FixtureResponse::sse(200, sse).with_header("Mcp-Session-Id", "staged-session"),
        FixtureResponse::empty(202),
        FixtureResponse::empty(202),
    ])
    .await;
    let client = McpStreamableHttpClient::connect(
        Arc::new(PlanAuthorizer::direct(server.endpoint())),
        &McpStreamableHttpHeaderConfig::default(),
        &MapHeaderEnvironment::default(),
        McpRemoteClientCapabilities::empty(),
        McpStreamableHttpLimits::default(),
    )
    .await
    .expect("initialize with ping");
    assert_eq!(client.lifecycle().await, McpStreamableHttpLifecycle::Ready);
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].contains("mcp-session-id: staged-session"));
    assert!(requests[1].contains("ping-init"));
    assert!(requests[2].contains("notifications/initialized"));
}

#[test]
fn streamable_http_auth_parser_handles_realm_duplicates_and_oversize_without_fetch() {
    let mut valid = HeaderMap::new();
    valid.insert(
        WWW_AUTHENTICATE,
        HeaderValue::from_static(
            "Bearer realm=\"mcp\", ReSoUrCe_MeTaDaTa=\"https://auth.example/meta\"",
        ),
    );
    assert!(matches!(
        auth::classify_unauthorized(&valid, false),
        Err(McpStreamableHttpError::OAuthUnsupported)
    ));
    let mut duplicate = HeaderMap::new();
    duplicate.append(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"one\""),
    );
    duplicate.append(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"two\""),
    );
    assert!(matches!(
        auth::classify_unauthorized(&duplicate, false),
        Err(McpStreamableHttpError::InvalidAuthenticationChallenge)
    ));
    let mut oversize = HeaderMap::new();
    oversize.insert(
        WWW_AUTHENTICATE,
        HeaderValue::from_str(&format!("Bearer realm=\"{}\"", "x".repeat(5000))).expect("header"),
    );
    assert!(matches!(
        auth::classify_unauthorized(&oversize, false),
        Err(McpStreamableHttpError::InvalidAuthenticationChallenge)
    ));
}

#[tokio::test]
async fn streamable_http_header_preflight_precedes_authorization() {
    let server = FixtureServer::start(Vec::new()).await;
    let authorizer = PlanAuthorizer::direct(server.endpoint());
    let calls = authorizer.call_count();
    let config = McpStreamableHttpHeaderConfig {
        bearer_token_env_var: Some("MISSING_TOKEN".to_owned()),
        ..Default::default()
    };
    let error = McpStreamableHttpClient::connect(
        Arc::new(authorizer),
        &config,
        &MapHeaderEnvironment::default(),
        McpRemoteClientCapabilities::empty(),
        McpStreamableHttpLimits::default(),
    )
    .await
    .expect_err("missing credential before authorization");
    assert!(matches!(
        error,
        McpStreamableHttpError::AuthenticationRequired
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn streamable_http_stale_resolved_header_binding_has_zero_authorization_or_socket() {
    let server = FixtureServer::start(Vec::new()).await;
    let endpoint = server.endpoint();
    let config = McpStreamableHttpHeaderConfig {
        from_env: BTreeMap::from([("X-Trace".to_owned(), "TRACE".to_owned())]),
        ..Default::default()
    };
    let old_environment = MapHeaderEnvironment(BTreeMap::from([(
        "TRACE".to_owned(),
        SecretString::new("old-value"),
    )]));
    let old_prepared = PreparedMcpStreamableHttpHeaders::prepare(
        SecretString::new(endpoint.clone()),
        &config,
        &old_environment,
    )
    .expect("old binding");
    let authorizer = PlanAuthorizer::direct_with_bindings(
        endpoint,
        "fixture-profile",
        old_prepared.live_header_fingerprint(),
    );
    let calls = authorizer.call_count();
    let new_environment = MapHeaderEnvironment(BTreeMap::from([(
        "TRACE".to_owned(),
        SecretString::new("new-value"),
    )]));
    let error = McpStreamableHttpClient::connect(
        Arc::new(authorizer),
        &config,
        &new_environment,
        McpRemoteClientCapabilities::empty(),
        McpStreamableHttpLimits::default(),
    )
    .await
    .expect_err("stale live HMAC");
    assert!(matches!(
        error,
        McpStreamableHttpError::ConfigurationInvalid
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn streamable_http_timeout_disconnect_and_5xx_never_retry() {
    let cases = vec![
        (
            FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false))
                .with_delay(Duration::from_millis(200)),
            McpStreamableHttpLimits {
                response_timeout: Duration::from_millis(50),
                ..McpStreamableHttpLimits::default()
            },
            "timeout",
        ),
        (
            FixtureResponse::disconnect(),
            McpStreamableHttpLimits::default(),
            "disconnect",
        ),
        (
            FixtureResponse::empty(503),
            McpStreamableHttpLimits::default(),
            "service",
        ),
    ];
    for (response, limits, label) in cases {
        let server = FixtureServer::start(vec![response]).await;
        let authorizer = PlanAuthorizer::direct(server.endpoint());
        let calls = authorizer.call_count();
        let error = McpStreamableHttpClient::connect(
            Arc::new(authorizer),
            &McpStreamableHttpHeaderConfig::default(),
            &MapHeaderEnvironment::default(),
            McpRemoteClientCapabilities::empty(),
            limits,
        )
        .await
        .expect_err(label);
        assert!(matches!(
            error,
            McpStreamableHttpError::Timeout
                | McpStreamableHttpError::Transport
                | McpStreamableHttpError::ServiceUnavailable
        ));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{label}");
        assert_eq!(server.requests().len(), 1, "{label}");
    }
}

struct DelayedNthAuthorizer {
    inner: PlanAuthorizer,
    calls: Arc<AtomicUsize>,
    entered_delay: Arc<tokio::sync::Notify>,
    delayed_ordinal: usize,
}

impl DelayedNthAuthorizer {
    fn new(inner: PlanAuthorizer, delayed_ordinal: usize) -> Self {
        Self {
            inner,
            calls: Arc::new(AtomicUsize::new(0)),
            entered_delay: Arc::new(tokio::sync::Notify::new()),
            delayed_ordinal,
        }
    }
}

#[async_trait]
impl McpStreamableHttpDestinationAuthorizer for DelayedNthAuthorizer {
    fn endpoint(&self) -> SecretString {
        self.inner.endpoint()
    }

    fn profile_config_proxy_fingerprint(&self) -> String {
        self.inner.profile_config_proxy_fingerprint()
    }

    fn live_header_fingerprint(&self) -> String {
        self.inner.live_header_fingerprint()
    }

    async fn authorize_destination(
        &self,
    ) -> Result<McpStreamableHttpAuthorizedDialPlan, McpStreamableHttpDestinationError> {
        let ordinal = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if ordinal == self.delayed_ordinal {
            self.entered_delay.notify_one();
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        self.inner.authorize_destination().await
    }
}

fn empty_tool(name: &str) -> McpRemoteTool {
    McpRemoteTool {
        name: name.to_owned(),
        description: None,
        input_schema: json!({"type":"object","properties":{},"additionalProperties":false}),
        output_schema: None,
        task_support: None,
    }
}

#[tokio::test]
async fn streamable_http_cancel_before_first_body_poll_sends_no_cancel_notification() {
    let server = FixtureServer::start(vec![
        FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false))
            .with_header("Mcp-Session-Id", "live-session"),
        FixtureResponse::empty(202),
    ])
    .await;
    let delayed = Arc::new(DelayedNthAuthorizer::new(
        PlanAuthorizer::direct(server.endpoint()),
        3,
    ));
    let calls = Arc::clone(&delayed.calls);
    let entered = Arc::clone(&delayed.entered_delay);
    let client = McpStreamableHttpClient::connect(
        delayed,
        &McpStreamableHttpHeaderConfig::default(),
        &MapHeaderEnvironment::default(),
        McpRemoteClientCapabilities::empty(),
        McpStreamableHttpLimits::default(),
    )
    .await
    .expect("connect");
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let task = tokio::spawn(async move {
        client
            .call_tool(&empty_tool("never-sent"), json!({}), Some(&handle), &|| {
                true
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("authorizer entered");
    owner.request_cancel();
    assert!(matches!(
        task.await.expect("join"),
        Err(McpStreamableHttpError::Cancelled)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("notifications/cancelled"))
    );
}

#[tokio::test]
async fn streamable_http_post_send_timeout_does_not_retry_or_spawn_cancel_socket() {
    let server = FixtureServer::start(vec![
        FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false))
            .with_header("Mcp-Session-Id", "live-session"),
        FixtureResponse::empty(202),
        FixtureResponse::json(
            200,
            json!({"jsonrpc":"2.0","id":2,"result":{"content":[]}}).to_string(),
        )
        .with_delay(Duration::from_millis(300)),
    ])
    .await;
    let authorizer = PlanAuthorizer::direct(server.endpoint());
    let calls = authorizer.call_count();
    let client = McpStreamableHttpClient::connect(
        Arc::new(authorizer),
        &McpStreamableHttpHeaderConfig::default(),
        &MapHeaderEnvironment::default(),
        McpRemoteClientCapabilities::empty(),
        McpStreamableHttpLimits {
            response_timeout: Duration::from_millis(75),
            ..McpStreamableHttpLimits::default()
        },
    )
    .await
    .expect("connect");
    assert!(matches!(
        client
            .call_tool(&empty_tool("slow"), json!({}), None, &|| true)
            .await,
        Err(McpStreamableHttpError::Timeout)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains("tools/call"))
            .count(),
        1
    );
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("notifications/cancelled"))
    );
}

#[tokio::test]
async fn streamable_http_plain_401_is_typed_and_never_retried() {
    let server = FixtureServer::start(vec![FixtureResponse::empty(401)]).await;
    let authorizer = PlanAuthorizer::direct(server.endpoint());
    let calls = authorizer.call_count();
    let error = McpStreamableHttpClient::connect(
        Arc::new(authorizer),
        &McpStreamableHttpHeaderConfig::default(),
        &MapHeaderEnvironment::default(),
        McpRemoteClientCapabilities::empty(),
        McpStreamableHttpLimits::default(),
    )
    .await
    .expect_err("plain 401");
    assert!(matches!(
        error,
        McpStreamableHttpError::AuthenticationRequired
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn streamable_http_oauth_challenge_http_fixtures_never_fetch_metadata_or_retry() {
    let challenges = vec![
        (
            "Bearer realm=\"mcp\", resource_metadata=\"https://auth.example/meta\"".to_owned(),
            "oauth",
        ),
        (
            "Bearer resource_metadata=\"http://127.0.0.1/meta\"".to_owned(),
            "invalid",
        ),
        (format!("Bearer realm=\"{}\"", "x".repeat(5000)), "oversize"),
    ];
    for (challenge, kind) in challenges {
        let server = FixtureServer::start(vec![
            FixtureResponse::empty(401).with_header("WWW-Authenticate", challenge),
        ])
        .await;
        let authorizer = PlanAuthorizer::direct(server.endpoint());
        let calls = authorizer.call_count();
        let error = McpStreamableHttpClient::connect(
            Arc::new(authorizer),
            &McpStreamableHttpHeaderConfig::default(),
            &MapHeaderEnvironment::default(),
            McpRemoteClientCapabilities::empty(),
            McpStreamableHttpLimits::default(),
        )
        .await
        .expect_err(kind);
        match kind {
            "oauth" => assert!(matches!(error, McpStreamableHttpError::OAuthUnsupported)),
            _ => assert!(matches!(
                error,
                McpStreamableHttpError::InvalidAuthenticationChallenge
            )),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests.iter().all(|request| !request.contains("/meta")));
    }
}

#[tokio::test]
async fn streamable_http_post_statuses_are_typed_and_never_replayed() {
    for status in [400, 404, 405, 406, 415, 307] {
        let server = FixtureServer::start(vec![FixtureResponse::empty(status)]).await;
        let authorizer = PlanAuthorizer::direct(server.endpoint());
        let calls = authorizer.call_count();
        let error = McpStreamableHttpClient::connect(
            Arc::new(authorizer),
            &McpStreamableHttpHeaderConfig::default(),
            &MapHeaderEnvironment::default(),
            McpRemoteClientCapabilities::empty(),
            McpStreamableHttpLimits::default(),
        )
        .await
        .expect_err("typed POST status");
        assert!(matches!(
            error,
            McpStreamableHttpError::UnexpectedHttpStatus { status: actual }
                if actual == status
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn streamable_http_ready_response_cannot_replace_live_session_id() {
    let (client, server) = connect_fixture(
        vec![
            FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false))
                .with_header("Mcp-Session-Id", "old-session"),
            FixtureResponse::empty(202),
            FixtureResponse::json(
                200,
                json!({"jsonrpc":"2.0","id":2,"result":{"tools":[]}}).to_string(),
            )
            .with_header("Mcp-Session-Id", "attacker-session"),
            FixtureResponse::json(
                200,
                json!({"jsonrpc":"2.0","id":3,"result":{"tools":[]}}).to_string(),
            ),
        ],
        McpRemoteClientCapabilities::empty(),
    )
    .await;
    assert!(matches!(
        client.list_tools().await,
        Err(McpStreamableHttpError::InvalidSessionId)
    ));
    client.list_tools().await.expect("old session remains live");
    assert!(server.requests()[3].contains("mcp-session-id: old-session"));
}

#[tokio::test]
async fn streamable_http_get_404_clears_live_session() {
    let (client, _) = connect_fixture(
        vec![
            FixtureResponse::json(200, initialize_json(1, LATEST_PROTOCOL_VERSION, false))
                .with_header("Mcp-Session-Id", "old-session"),
            FixtureResponse::empty(202),
            FixtureResponse::empty(404),
        ],
        McpRemoteClientCapabilities::empty(),
    )
    .await;
    assert!(matches!(
        client.probe_get_listener().await,
        Err(McpStreamableHttpError::SessionExpired)
    ));
    assert_eq!(
        client.lifecycle().await,
        McpStreamableHttpLifecycle::Disconnected
    );
}
