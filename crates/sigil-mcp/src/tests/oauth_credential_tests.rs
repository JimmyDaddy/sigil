use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use base64::{Engine as _, engine::general_purpose};

use super::*;

#[derive(Clone)]
struct RecordedRequest {
    purpose: McpOAuthHttpPurpose,
    destination: String,
    headers: Vec<(String, String)>,
    body: String,
}

#[derive(Default)]
struct QueueExecutor {
    responses:
        Mutex<VecDeque<Result<McpOAuthHttpResponse, super::super::oauth::McpOAuthTransportError>>>,
    requests: Mutex<Vec<RecordedRequest>>,
}

impl QueueExecutor {
    fn with_responses(
        responses: Vec<Result<McpOAuthHttpResponse, super::super::oauth::McpOAuthTransportError>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        })
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
    ) -> Result<McpOAuthHttpResponse, super::super::oauth::McpOAuthTransportError> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(RecordedRequest {
                purpose: request.purpose(),
                destination: request.destination().to_owned(),
                headers: request
                    .headers()
                    .iter()
                    .map(|(name, value)| (name.clone(), value.expose_secret().to_owned()))
                    .collect(),
                body: request.body().unwrap_or_default().to_owned(),
            });
        self.responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .expect("queued response")
    }
}

fn json_response(status: u16, value: serde_json::Value) -> McpOAuthHttpResponse {
    McpOAuthHttpResponse::new(
        status,
        Some("application/json".to_owned()),
        SecretString::new(value.to_string()),
    )
}

fn scope() -> McpOAuthCredentialScope {
    McpOAuthCredentialScope::new(
        "github",
        "https://mcp.example/public/mcp",
        "https://auth.example/tenant",
        "sigil-client",
        vec!["files:write".to_owned(), "files:read".to_owned()],
    )
    .expect("scope")
}

fn record(_now: u64, expires_at: Option<u64>, revocation: bool) -> McpOAuthCredentialRecord {
    McpOAuthCredentialRecord {
        scope: scope(),
        access_token: Some(SecretString::new("access-token-canary")),
        refresh_token: Some(SecretString::new("refresh-token-canary")),
        expires_at_epoch_secs: expires_at,
        token_type: "Bearer".to_owned(),
        client_secret: None,
        token_endpoint_auth_method: "none".to_owned(),
        registration_access_token: None,
        registration_client_uri: None,
        client_id_issued_at: None,
        client_secret_expires_at: None,
        token_endpoint: "https://auth.example/token".to_owned(),
        revocation_endpoint: revocation.then(|| "https://auth.example/revoke".to_owned()),
        rotation_id: Uuid::new_v4().to_string(),
    }
}

#[test]
fn scope_and_record_round_trip_are_exact_bounded_and_redacted() {
    let first = scope();
    let reordered = McpOAuthCredentialScope::new(
        "github",
        "https://mcp.example/public/mcp",
        "https://auth.example/tenant",
        "sigil-client",
        vec!["files:read".to_owned(), "files:write".to_owned()],
    )
    .expect("scope");
    assert_eq!(first, reordered);
    assert_eq!(first.scopes(), &["files:read", "files:write"]);
    assert!(!format!("{first:?}").contains("auth.example"));

    let original = record(1_000, Some(2_000), true);
    let encoded = encode_record(&original).expect("encode");
    let decoded = decode_record(&first, &encoded).expect("decode");
    assert_eq!(decoded.scope(), &first);
    assert_eq!(decoded.status(1_000), McpOAuthCredentialStatus::Present);
    let debug = format!("{decoded:?}");
    assert!(!debug.contains("access-token-canary"));
    assert!(!debug.contains("refresh-token-canary"));

    let other = McpOAuthCredentialScope::new(
        "other",
        first.resource(),
        first.issuer(),
        first.client_id(),
        first.scopes().to_vec(),
    )
    .expect("other scope");
    assert!(matches!(
        decode_record(&other, &encoded),
        Err(McpOAuthCredentialError::InvalidRecord)
    ));
    assert!(matches!(
        decode_record(&first, &vec![b'x'; MAX_CREDENTIAL_RECORD_BYTES + 1]),
        Err(McpOAuthCredentialError::InvalidRecord)
    ));
}

#[test]
fn configured_lookup_locates_only_an_exact_server_resource_and_public_client() {
    let lookup = McpOAuthCredentialLookup::new(
        "github",
        "https://mcp.example/public/mcp",
        Some("sigil-client".to_owned()),
        vec!["files:write".to_owned(), "files:read".to_owned()],
    )
    .expect("lookup");
    let reordered = McpOAuthCredentialLookup::new(
        "github",
        "https://mcp.example/public/mcp",
        Some("sigil-client".to_owned()),
        vec!["files:read".to_owned(), "files:write".to_owned()],
    )
    .expect("lookup");
    assert_eq!(lookup, reordered);
    assert!(lookup.accepts(&scope()));
    let encoded = encode_locator(&scope()).expect("locator");
    assert_eq!(decode_locator(&lookup, &encoded).expect("decode"), scope());

    let other_client = McpOAuthCredentialLookup::new(
        "github",
        "https://mcp.example/public/mcp",
        Some("other-client".to_owned()),
        Vec::new(),
    )
    .expect("lookup");
    assert!(!other_client.accepts(&scope()));
    assert!(matches!(
        decode_locator(&other_client, &encoded),
        Err(McpOAuthCredentialError::InvalidRecord)
    ));
    assert!(!format!("{lookup:?}").contains("mcp.example"));
}

#[test]
fn expiry_and_rotation_change_only_safe_status_and_fingerprint() {
    let original = record(1_000, Some(1_100), false);
    assert_eq!(original.status(1_000), McpOAuthCredentialStatus::Present);
    assert_eq!(original.status(1_050), McpOAuthCredentialStatus::Expiring);
    assert_eq!(original.status(1_100), McpOAuthCredentialStatus::Expired);
    let snapshot = original
        .snapshot("static-fingerprint", 1_000)
        .expect("snapshot");
    let same = original
        .snapshot("static-fingerprint", 1_000)
        .expect("snapshot");
    assert_eq!(snapshot.live_fingerprint(), same.live_fingerprint());
    assert_eq!(
        snapshot.authorization().expose_secret(),
        "Bearer access-token-canary"
    );
    assert!(!format!("{snapshot:?}").contains("access-token-canary"));

    let token = McpOAuthTokenResponse {
        access_token: SecretString::new("rotated-access-canary"),
        refresh_token: Some(SecretString::new("rotated-refresh-canary")),
        expires_in_secs: Some(3_600),
        scopes: vec!["files:read".to_owned()],
    };
    let rotated = original.rotated(&token, 1_020).expect("rotate");
    let rotated_snapshot = rotated
        .snapshot("static-fingerprint", 1_020)
        .expect("snapshot");
    assert_ne!(
        snapshot.live_fingerprint(),
        rotated_snapshot.live_fingerprint()
    );
    assert!(!format!("{rotated:?}").contains("rotated-refresh-canary"));

    let unusable = rotated.without_usable_tokens(1_030);
    assert_eq!(unusable.status(1_030), McpOAuthCredentialStatus::Expired);
    assert!(!unusable.has_refresh_token());
    assert!(matches!(
        unusable.snapshot("static-fingerprint", 1_030),
        Err(McpOAuthCredentialError::AuthenticationRequired)
    ));
}

#[tokio::test]
async fn refresh_binds_resource_client_scope_and_rotates_without_secret_debug() {
    let original = record(1_000, Some(1_001), false);
    let executor = QueueExecutor::with_responses(vec![Ok(json_response(
        200,
        serde_json::json!({
            "access_token": "next-access-canary",
            "refresh_token": "next-refresh-canary",
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "files:read files:write"
        }),
    ))]);
    let token = refresh_oauth_credential(executor.as_ref(), &original)
        .await
        .expect("refresh");
    let next = original.rotated(&token, 1_010).expect("rotate");
    assert_eq!(next.status(1_010), McpOAuthCredentialStatus::Present);
    let requests = executor.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].purpose, McpOAuthHttpPurpose::TokenRefresh);
    assert_eq!(requests[0].destination, "https://auth.example/token");
    assert!(requests[0].headers.is_empty());
    assert!(requests[0].body.contains("grant_type=refresh_token"));
    assert!(requests[0].body.contains("refresh-token-canary"));
    assert!(
        requests[0]
            .body
            .contains("resource=https%3A%2F%2Fmcp.example%2Fpublic%2Fmcp")
    );
    assert!(
        requests[0]
            .body
            .contains("scope=files%3Aread+files%3Awrite")
    );
    assert!(!format!("{next:?}").contains("next-access-canary"));
}

#[tokio::test]
async fn invalid_refresh_transport_and_revoke_are_one_attempt_and_typed() {
    let original = record(1_000, Some(1_001), true);
    let invalid = QueueExecutor::with_responses(vec![Ok(json_response(
        400,
        serde_json::json!({"error": "invalid_grant", "error_description": "secret"}),
    ))]);
    assert!(matches!(
        refresh_oauth_credential(invalid.as_ref(), &original).await,
        Err(McpOAuthCredentialError::InvalidRefresh)
    ));
    assert_eq!(invalid.requests().len(), 1);

    let transport = QueueExecutor::with_responses(vec![Err(
        super::super::oauth::McpOAuthTransportError::Transport,
    )]);
    assert!(matches!(
        refresh_oauth_credential(transport.as_ref(), &original).await,
        Err(McpOAuthCredentialError::Transport)
    ));
    assert_eq!(transport.requests().len(), 1);

    let revoke = QueueExecutor::with_responses(vec![Ok(McpOAuthHttpResponse::new(
        503,
        None,
        SecretString::new(String::new()),
    ))]);
    assert!(matches!(
        revoke_oauth_credential(revoke.as_ref(), &original).await,
        Err(McpOAuthCredentialError::RevocationRejected)
    ));
    assert_eq!(revoke.requests().len(), 1);

    let no_endpoint = record(1_000, Some(1_001), false);
    let unused = QueueExecutor::with_responses(Vec::new());
    assert_eq!(
        revoke_oauth_credential(unused.as_ref(), &no_endpoint)
            .await
            .expect("not advertised"),
        McpOAuthRevocationOutcome::NotAdvertised
    );
    assert!(unused.requests().is_empty());
}

#[tokio::test]
async fn confidential_client_refresh_uses_form_encoded_basic_and_honors_secret_expiry() {
    let mut confidential = record(1_000, Some(1_001), false);
    confidential.scope = McpOAuthCredentialScope::new(
        "github",
        "https://mcp.example/public/mcp",
        "https://auth.example/tenant",
        "sigil:client",
        vec!["files:read".to_owned(), "files:write".to_owned()],
    )
    .expect("scope");
    confidential.client_secret = Some(SecretString::new("sec:ret value"));
    confidential.token_endpoint_auth_method = "client_secret_basic".to_owned();
    confidential.client_secret_expires_at = Some(2_000);
    assert!(confidential.can_refresh(1_999));
    assert!(!confidential.can_refresh(2_000));

    let executor = QueueExecutor::with_responses(vec![Ok(json_response(
        200,
        serde_json::json!({
            "access_token": "next-access",
            "token_type": "Bearer",
            "scope": "files:read files:write"
        }),
    ))]);
    refresh_oauth_credential(executor.as_ref(), &confidential)
        .await
        .expect("refresh");
    let requests = executor.requests();
    let header = requests[0]
        .headers
        .iter()
        .find_map(|(name, value)| (name == "authorization").then_some(value))
        .expect("authorization header");
    let decoded = general_purpose::STANDARD
        .decode(header.strip_prefix("Basic ").expect("Basic scheme"))
        .expect("base64");
    assert_eq!(decoded, b"sigil%3Aclient:sec%3Aret+value");
}
