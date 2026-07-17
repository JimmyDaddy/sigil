use std::{collections::VecDeque, sync::Mutex};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use super::*;

#[derive(Debug, Clone)]
struct RecordedRequest {
    method: McpOAuthHttpMethod,
    purpose: McpOAuthHttpPurpose,
    destination: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
}

#[derive(Default)]
struct QueueExecutor {
    responses: Mutex<VecDeque<Result<McpOAuthHttpResponse, McpOAuthTransportError>>>,
    requests: Mutex<Vec<RecordedRequest>>,
}

impl QueueExecutor {
    fn new(responses: Vec<McpOAuthHttpResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(Ok).collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

#[async_trait]
impl McpOAuthHttpExecutor for QueueExecutor {
    async fn execute(
        &self,
        request: McpOAuthHttpRequest,
    ) -> Result<McpOAuthHttpResponse, McpOAuthTransportError> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(RecordedRequest {
                method: request.method(),
                purpose: request.purpose(),
                destination: request.destination().to_owned(),
                headers: request
                    .headers()
                    .iter()
                    .map(|(name, value)| (name.clone(), value.expose_secret().to_owned()))
                    .collect(),
                body: request.body().map(str::to_owned),
            });
        self.responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .expect("fixture response")
    }
}

fn json_response(status: u16, value: serde_json::Value) -> McpOAuthHttpResponse {
    McpOAuthHttpResponse::new(
        status,
        Some("application/json".to_owned()),
        SecretString::new(value.to_string()),
    )
}

fn empty_response(status: u16) -> McpOAuthHttpResponse {
    McpOAuthHttpResponse::new(status, None, SecretString::new(String::new()))
}

fn discovery(token_auth_methods: &[&str], registration: bool) -> McpOAuthDiscovery {
    McpOAuthDiscovery {
        resource: McpOAuthResource::parse("https://resource.example/public/mcp").expect("resource"),
        resource_metadata_endpoint: Url::parse(
            "https://resource.example/.well-known/oauth-protected-resource/public/mcp",
        )
        .expect("metadata URL"),
        issuer: Url::parse("https://auth.example/tenant").expect("issuer"),
        authorization_endpoint: Url::parse("https://auth.example/authorize").expect("authorize"),
        token_endpoint: Url::parse("https://auth.example/token").expect("token"),
        registration_endpoint: registration
            .then(|| Url::parse("https://auth.example/register").expect("register")),
        revocation_endpoint: Some(Url::parse("https://auth.example/revoke").expect("revoke")),
        token_auth_methods: token_auth_methods
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        challenge_scopes: vec!["files:read".to_owned()],
        resource_scopes: vec!["files:read".to_owned(), "files:write".to_owned()],
    }
}

fn public_client() -> McpOAuthClientRegistration {
    McpOAuthClientRegistration {
        client_id: "sigil-public".to_owned(),
        client_secret: None,
        auth_method: ClientAuthMethod::None,
        registration_access_token: None,
        registration_client_uri: None,
        client_id_issued_at: None,
        client_secret_expires_at: None,
    }
}

#[tokio::test]
async fn discovery_uses_rfc9728_and_mcp_metadata_fallback_order() {
    let challenge = McpOAuthChallenge::parse(
        "Bearer realm=\"mcp\", scope=\"files:read\"",
        SecretString::new("https://resource.example/public/mcp"),
    )
    .expect("challenge")
    .expect("bearer");
    let executor = QueueExecutor::new(vec![
        empty_response(404),
        json_response(
            200,
            serde_json::json!({
                "resource": "https://resource.example/public/mcp",
                "authorization_servers": ["https://auth.example/tenant"],
                "scopes_supported": ["files:read", "files:write"]
            }),
        ),
        empty_response(404),
        empty_response(404),
        json_response(
            200,
            serde_json::json!({
                "issuer": "https://auth.example/tenant",
                "authorization_endpoint": "https://auth.example/authorize",
                "token_endpoint": "https://auth.example/token",
                "registration_endpoint": "https://auth.example/register",
                "revocation_endpoint": "https://auth.example/revoke",
                "response_types_supported": ["code"],
                "grant_types_supported": ["authorization_code"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none"],
                "protected_resources": ["https://resource.example/public/mcp"]
            }),
        ),
    ]);

    let discovered = discover_oauth_authorization_server(&executor, &challenge)
        .await
        .expect("discovery");
    assert_eq!(discovered.issuer(), "https://auth.example/tenant");
    assert_eq!(
        discovered.resource_metadata_endpoint(),
        "https://resource.example/.well-known/oauth-protected-resource"
    );
    assert_eq!(discovered.resource_scopes().len(), 2);
    let intent = McpOAuthClientIntent::new(
        Some("sigil-public".to_owned()),
        vec!["configured:scope".to_owned()],
    )
    .expect("intent");
    assert_eq!(
        discovered.requested_scopes(&intent).expect("scopes"),
        ["files:read"]
    );

    let requests = executor.requests();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests
            .iter()
            .map(|request| request.destination.as_str())
            .collect::<Vec<_>>(),
        [
            "https://resource.example/.well-known/oauth-protected-resource/public/mcp",
            "https://resource.example/.well-known/oauth-protected-resource",
            "https://auth.example/.well-known/oauth-authorization-server/tenant",
            "https://auth.example/.well-known/openid-configuration/tenant",
            "https://auth.example/tenant/.well-known/openid-configuration",
        ]
    );
    assert!(
        requests
            .iter()
            .all(|request| request.method == McpOAuthHttpMethod::Get)
    );
    assert_eq!(
        requests[0].purpose,
        McpOAuthHttpPurpose::ProtectedResourceMetadata
    );
    assert_eq!(
        requests[4].purpose,
        McpOAuthHttpPurpose::AuthorizationServerMetadata
    );
}

#[tokio::test]
async fn discovery_rejects_resource_issuer_pkce_and_ambiguity_drift() {
    let challenge = McpOAuthChallenge::parse(
        "Bearer resource_metadata=\"https://resource.example/meta\"",
        SecretString::new("https://resource.example/mcp"),
    )
    .expect("challenge")
    .expect("bearer");
    for resource_document in [
        serde_json::json!({
            "resource": "https://other.example/mcp",
            "authorization_servers": ["https://auth.example"]
        }),
        serde_json::json!({
            "resource": "https://resource.example/mcp",
            "authorization_servers": ["https://one.example", "https://two.example"]
        }),
    ] {
        let executor = QueueExecutor::new(vec![json_response(200, resource_document)]);
        assert!(
            discover_oauth_authorization_server(&executor, &challenge)
                .await
                .is_err()
        );
        assert_eq!(executor.requests().len(), 1);
    }

    let executor = QueueExecutor::new(vec![
        json_response(
            200,
            serde_json::json!({
                "resource": "https://resource.example/mcp",
                "authorization_servers": ["https://auth.example"]
            }),
        ),
        json_response(
            200,
            serde_json::json!({
                "issuer": "https://auth.example",
                "authorization_endpoint": "https://auth.example/authorize",
                "token_endpoint": "https://auth.example/token",
                "response_types_supported": ["code"],
                "code_challenge_methods_supported": ["plain"],
                "token_endpoint_auth_methods_supported": ["none"]
            }),
        ),
    ]);
    assert_eq!(
        discover_oauth_authorization_server(&executor, &challenge)
            .await
            .expect_err("plain PKCE must fail"),
        McpOAuthProtocolError::UnsupportedAuthorizationServer
    );
}

#[tokio::test]
async fn static_and_dynamic_client_paths_validate_public_and_confidential_bindings() {
    let intent =
        McpOAuthClientIntent::new(Some("sigil-public".to_owned()), vec![]).expect("intent");
    let no_network = QueueExecutor::default();
    let client = prepare_oauth_client(
        &no_network,
        &discovery(&["none"], false),
        &intent,
        "http://127.0.0.1:43123/callback",
    )
    .await
    .expect("static client");
    assert_eq!(client.client_id(), "sigil-public");
    assert!(!client.has_client_secret());
    assert!(no_network.requests().is_empty());

    let secret = "dcr-client-secret-canary";
    let registration_token = "dcr-registration-token-canary";
    let dcr = QueueExecutor::new(vec![json_response(
        201,
        serde_json::json!({
            "client_id": "dynamic-client",
            "client_secret": secret,
            "token_endpoint_auth_method": "client_secret_post",
            "registration_access_token": registration_token,
            "registration_client_uri": "https://auth.example/clients/1",
            "client_id_issued_at": 7,
            "client_secret_expires_at": 11
        }),
    )]);
    let dynamic = prepare_oauth_client(
        &dcr,
        &discovery(&["client_secret_post"], true),
        &McpOAuthClientIntent::new(None, vec![]).expect("DCR intent"),
        "http://127.0.0.1:43123/callback",
    )
    .await
    .expect("DCR");
    assert_eq!(dynamic.client_id(), "dynamic-client");
    assert_eq!(
        dynamic.client_secret().expect("secret").expose_secret(),
        secret
    );
    assert_eq!(
        dynamic
            .registration_access_token()
            .expect("registration token")
            .expose_secret(),
        registration_token
    );
    assert_eq!(
        dynamic.registration_client_uri(),
        Some("https://auth.example/clients/1")
    );
    assert_eq!(dynamic.client_id_issued_at(), Some(7));
    assert_eq!(dynamic.client_secret_expires_at(), Some(11));
    let debug = format!("{dynamic:?}");
    assert!(!debug.contains(secret));
    assert!(!debug.contains(registration_token));
    let requests = dcr.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, McpOAuthHttpMethod::Post);
    assert_eq!(
        requests[0].purpose,
        McpOAuthHttpPurpose::DynamicClientRegistration
    );
    assert!(
        requests[0]
            .body
            .as_deref()
            .expect("DCR body")
            .contains("client_secret_post")
    );
}

#[tokio::test]
async fn pkce_callback_and_token_exchange_bind_exact_state_resource_and_secrets() {
    let discovered = discovery(&["none"], false);
    let mut pending = McpOAuthPendingAuthorization::new(
        discovered.clone(),
        public_client(),
        vec!["files:read".to_owned()],
        "http://127.0.0.1:43123/callback",
    )
    .expect("pending");
    let authorization_url = pending.authorization_url();
    let parsed = Url::parse(authorization_url.expose_secret()).expect("authorization URL");
    let parameters = parsed
        .query_pairs()
        .into_owned()
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        parameters.get("response_type").map(String::as_str),
        Some("code")
    );
    assert_eq!(
        parameters.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert_eq!(
        parameters.get("resource").map(String::as_str),
        Some("https://resource.example/public/mcp")
    );
    assert!(!parameters.contains_key("code_verifier"));
    assert!(!format!("{pending:?}").contains(parameters.get("state").expect("state")));

    let mut wrong = McpOAuthPendingAuthorization::new(
        discovered.clone(),
        public_client(),
        vec![],
        "http://127.0.0.1:43123/callback",
    )
    .expect("wrong-state pending");
    assert!(matches!(
        wrong.complete_callback(SecretString::new(
            "http://127.0.0.1:43123/callback?code=one&state=wrong",
        )),
        Err(McpOAuthProtocolError::InvalidAuthorizationResponse)
    ));
    assert!(matches!(
        wrong.complete_callback(SecretString::new(
            "http://127.0.0.1:43123/callback?code=two&state=wrong",
        )),
        Err(McpOAuthProtocolError::FlowConsumed)
    ));

    let mut wrong_origin = McpOAuthPendingAuthorization::new(
        discovered.clone(),
        public_client(),
        vec![],
        "http://127.0.0.1:43123/callback",
    )
    .expect("wrong-origin pending");
    assert!(matches!(
        wrong_origin.complete_callback(SecretString::new(
            "http://localhost:43123/callback?code=one&state=untrusted",
        )),
        Err(McpOAuthProtocolError::InvalidAuthorizationResponse)
    ));

    let mut rejected = McpOAuthPendingAuthorization::new(
        discovered.clone(),
        public_client(),
        vec![],
        "http://127.0.0.1:43123/callback",
    )
    .expect("rejected pending");
    let rejected_state = Url::parse(rejected.authorization_url().expose_secret())
        .expect("authorization URL")
        .query_pairs()
        .find_map(|(name, value)| (name == "state").then(|| value.into_owned()))
        .expect("state");
    assert!(matches!(
        rejected.complete_callback(SecretString::new(format!(
            "http://127.0.0.1:43123/callback?error=access_denied&state={rejected_state}&iss=https%3A%2F%2Fauth.example%2Ftenant"
        ))),
        Err(McpOAuthProtocolError::AuthorizationRejected)
    ));

    let state = parameters.get("state").expect("state");
    let code = "authorization-code-canary";
    let authorization = pending
        .complete_callback(SecretString::new(format!(
            "http://127.0.0.1:43123/callback?code={code}&state={state}&iss=https%3A%2F%2Fauth.example%2Ftenant"
        )))
        .expect("callback");
    assert!(!format!("{authorization:?}").contains(code));

    let access = "access-token-canary";
    let refresh = "refresh-token-canary";
    let token_executor = QueueExecutor::new(vec![json_response(
        200,
        serde_json::json!({
            "access_token": access,
            "refresh_token": refresh,
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "files:read"
        }),
    )]);
    let token = exchange_oauth_authorization_code(&token_executor, authorization)
        .await
        .expect("token exchange");
    assert_eq!(token.access_token().expose_secret(), access);
    assert_eq!(
        token.refresh_token().expect("refresh").expose_secret(),
        refresh
    );
    let debug = format!("{token:?}");
    assert!(!debug.contains(access));
    assert!(!debug.contains(refresh));
    let requests = token_executor.requests();
    let body = requests[0].body.as_deref().expect("token form");
    assert!(body.contains("grant_type=authorization_code"));
    assert!(body.contains("authorization-code-canary"));
    assert!(body.contains("code_verifier="));
    assert!(body.contains("resource=https%3A%2F%2Fresource.example%2Fpublic%2Fmcp"));
    assert!(requests[0].headers.is_empty());

    let mut escalated = McpOAuthPendingAuthorization::new(
        discovered,
        public_client(),
        vec!["files:read".to_owned()],
        "http://127.0.0.1:43123/callback",
    )
    .expect("pending");
    let escalated_state = Url::parse(escalated.authorization_url().expose_secret())
        .expect("authorization URL")
        .query_pairs()
        .find_map(|(name, value)| (name == "state").then(|| value.into_owned()))
        .expect("state");
    let escalated_code = escalated
        .complete_callback(SecretString::new(format!(
            "http://127.0.0.1:43123/callback?code=scope-code&state={escalated_state}"
        )))
        .expect("callback");
    let escalated_executor = QueueExecutor::new(vec![json_response(
        200,
        serde_json::json!({
            "access_token": "scope-token",
            "token_type": "Bearer",
            "scope": "files:write"
        }),
    )]);
    assert!(matches!(
        exchange_oauth_authorization_code(&escalated_executor, escalated_code).await,
        Err(McpOAuthProtocolError::TokenRejected)
    ));
}

#[tokio::test]
async fn loopback_listener_accepts_one_exact_callback_without_echoing_secrets() {
    let listener = McpOAuthLoopbackListener::bind().await.expect("listener");
    let redirect = listener.redirect_uri().to_owned();
    let mut pending = McpOAuthPendingAuthorization::new(
        discovery(&["none"], false),
        public_client(),
        vec![],
        &redirect,
    )
    .expect("pending");
    let authorization_url = pending.authorization_url();
    let state = Url::parse(authorization_url.expose_secret())
        .expect("authorization URL")
        .query_pairs()
        .find_map(|(name, value)| (name == "state").then(|| value.into_owned()))
        .expect("state");
    let address = Url::parse(&redirect).expect("redirect");
    let port = address.port().expect("port");
    let callback_state = state.clone();
    let client = async move {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .expect("connect callback");
        let request = format!(
            "GET /callback?code=loopback-code-canary&state={callback_state} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write callback");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .expect("read response");
        response
    };
    let (authorization, response) = tokio::join!(listener.receive(&mut pending), client);
    assert!(authorization.is_ok());
    assert!(response.contains("200 OK"));
    assert!(!response.contains("loopback-code-canary"));
    assert!(!response.contains(&state));
}

#[tokio::test]
async fn loopback_listener_rejects_slowloris_and_oversized_requests() {
    let slow_listener = McpOAuthLoopbackListener::bind().await.expect("listener");
    let slow_redirect = slow_listener.redirect_uri().to_owned();
    let mut slow_pending = McpOAuthPendingAuthorization::new_with_ttl(
        discovery(&["none"], false),
        public_client(),
        vec![],
        &slow_redirect,
        std::time::Duration::from_millis(25),
    )
    .expect("pending");
    let slow_port = Url::parse(&slow_redirect)
        .expect("redirect")
        .port()
        .expect("port");
    let slow_client = async move {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, slow_port))
            .await
            .expect("connect callback");
        stream
            .write_all(b"GET /callback?code=partial")
            .await
            .expect("write partial callback");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let (slow_result, ()) = tokio::join!(slow_listener.receive(&mut slow_pending), slow_client);
    assert!(matches!(
        slow_result,
        Err(McpOAuthProtocolError::FlowExpired)
    ));

    let large_listener = McpOAuthLoopbackListener::bind().await.expect("listener");
    let large_redirect = large_listener.redirect_uri().to_owned();
    let mut large_pending = McpOAuthPendingAuthorization::new(
        discovery(&["none"], false),
        public_client(),
        vec![],
        &large_redirect,
    )
    .expect("pending");
    let large_port = Url::parse(&large_redirect)
        .expect("redirect")
        .port()
        .expect("port");
    let large_client = async move {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, large_port))
            .await
            .expect("connect callback");
        let oversized = vec![b'x'; MAX_CALLBACK_REQUEST_BYTES + 1];
        let _ = stream.write_all(&oversized).await;
    };
    let (large_result, ()) = tokio::join!(large_listener.receive(&mut large_pending), large_client);
    assert!(matches!(
        large_result,
        Err(McpOAuthProtocolError::InvalidAuthorizationResponse)
    ));
}

#[test]
fn bounded_inputs_and_debug_surfaces_reject_or_redact_sensitive_values() {
    assert!(McpOAuthResource::parse("http://resource.example/mcp").is_err());
    assert!(McpOAuthClientIntent::new(Some("client with spaces".to_owned()), Vec::new()).is_err());
    assert!(
        McpOAuthClientIntent::new(
            Some("client".to_owned()),
            vec!["duplicate".to_owned(), "duplicate".to_owned()]
        )
        .is_err()
    );
    assert!(
        McpOAuthChallenge::parse(
            &format!("Bearer realm=\"{}\"", "x".repeat(MAX_CHALLENGE_BYTES)),
            SecretString::new("https://resource.example/mcp"),
        )
        .is_err()
    );
    let request = McpOAuthHttpRequest::post(
        &Url::parse("https://auth.example/token").expect("token URL"),
        McpOAuthHttpPurpose::TokenExchange,
        "application/x-www-form-urlencoded",
        vec![(
            "authorization".to_owned(),
            SecretString::new("Basic secret-header-canary"),
        )],
        SecretString::new("code=secret-code-canary"),
    );
    let debug = format!("{request:?}");
    assert!(!debug.contains("secret-header-canary"));
    assert!(!debug.contains("secret-code-canary"));
    assert!(!debug.contains("https://auth.example/token"));
}
