use std::{collections::VecDeque, sync::Mutex};

use async_trait::async_trait;
use sigil_kernel::{
    McpServerConfig, McpServerTransportConfig, McpStreamableHttpConfig, SecretString,
    config::McpOAuthConfig,
};
use sigil_mcp::{
    McpOAuthCredentialError, McpOAuthCredentialLocatorStore, McpOAuthCredentialLookup,
    McpOAuthCredentialRecord, McpOAuthCredentialScope, McpOAuthCredentialStatus,
    McpOAuthCredentialStore, McpOAuthHttpPurpose, McpOAuthHttpRequest, McpOAuthHttpResponse,
    McpOAuthProtocolError, McpOAuthTransportError,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use super::*;

#[derive(Default)]
struct MemoryCredentialStore {
    record: Mutex<Option<McpOAuthCredentialRecord>>,
    locator: Mutex<Option<McpOAuthCredentialScope>>,
}

#[async_trait]
impl McpOAuthCredentialStore for MemoryCredentialStore {
    async fn load(
        &self,
        scope: &McpOAuthCredentialScope,
    ) -> Result<Option<McpOAuthCredentialRecord>, McpOAuthCredentialError> {
        Ok(self
            .record
            .lock()
            .expect("credential lock")
            .as_ref()
            .filter(|record| record.scope() == scope)
            .cloned())
    }

    async fn store(
        &self,
        record: &McpOAuthCredentialRecord,
    ) -> Result<(), McpOAuthCredentialError> {
        *self.record.lock().expect("credential lock") = Some(record.clone());
        Ok(())
    }

    async fn delete(
        &self,
        scope: &McpOAuthCredentialScope,
    ) -> Result<bool, McpOAuthCredentialError> {
        let mut record = self.record.lock().expect("credential lock");
        if record
            .as_ref()
            .is_some_and(|record| record.scope() == scope)
        {
            record.take();
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[async_trait]
impl McpOAuthCredentialLocatorStore for MemoryCredentialStore {
    async fn load_located(
        &self,
        lookup: &McpOAuthCredentialLookup,
    ) -> Result<Option<McpOAuthCredentialRecord>, McpOAuthCredentialError> {
        let scope = self.locator.lock().expect("locator lock").clone();
        let Some(scope) = scope else {
            return Ok(None);
        };
        let _ = lookup;
        self.load(&scope).await
    }

    async fn store_locator(
        &self,
        lookup: &McpOAuthCredentialLookup,
        scope: &McpOAuthCredentialScope,
    ) -> Result<(), McpOAuthCredentialError> {
        let _ = lookup;
        *self.locator.lock().expect("locator lock") = Some(scope.clone());
        Ok(())
    }

    async fn delete_locator(
        &self,
        _lookup: &McpOAuthCredentialLookup,
    ) -> Result<bool, McpOAuthCredentialError> {
        Ok(self.locator.lock().expect("locator lock").take().is_some())
    }
}

#[derive(Debug)]
struct QueueExecutor {
    responses: Mutex<VecDeque<Result<McpOAuthHttpResponse, McpOAuthTransportError>>>,
    purposes: Mutex<Vec<McpOAuthHttpPurpose>>,
}

impl QueueExecutor {
    fn new(responses: Vec<McpOAuthHttpResponse>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().map(Ok).collect()),
            purposes: Mutex::new(Vec::new()),
        })
    }

    fn purposes(&self) -> Vec<McpOAuthHttpPurpose> {
        self.purposes.lock().expect("purpose lock").clone()
    }
}

#[async_trait]
impl McpOAuthHttpExecutor for QueueExecutor {
    async fn execute(
        &self,
        request: McpOAuthHttpRequest,
    ) -> Result<McpOAuthHttpResponse, McpOAuthTransportError> {
        self.purposes
            .lock()
            .expect("purpose lock")
            .push(request.purpose());
        self.responses
            .lock()
            .expect("response lock")
            .pop_front()
            .unwrap_or(Err(McpOAuthTransportError::Transport))
    }
}

fn response(status: u16, value: serde_json::Value) -> McpOAuthHttpResponse {
    McpOAuthHttpResponse::new(
        status,
        Some("application/json".to_owned()),
        SecretString::new(value.to_string()),
    )
}

fn discovery_responses() -> Vec<McpOAuthHttpResponse> {
    vec![
        response(
            200,
            serde_json::json!({
                "resource": "https://mcp.example/public/mcp",
                "authorization_servers": ["https://auth.example/tenant"],
                "scopes_supported": ["files:read"]
            }),
        ),
        response(
            200,
            serde_json::json!({
                "issuer": "https://auth.example/tenant",
                "authorization_endpoint": "https://auth.example/authorize",
                "token_endpoint": "https://auth.example/token",
                "revocation_endpoint": "https://auth.example/revoke",
                "response_types_supported": ["code"],
                "grant_types_supported": ["authorization_code", "refresh_token"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none"]
            }),
        ),
    ]
}

fn oauth_server() -> McpServerConfig {
    McpServerConfig {
        name: "github".to_owned(),
        transport: McpServerTransportConfig::StreamableHttp(McpStreamableHttpConfig {
            url: "https://mcp.example/public/mcp".to_owned(),
            http_headers: Default::default(),
            env_http_headers: Default::default(),
            bearer_token_env_var: None,
            oauth: Some(McpOAuthConfig {
                client_id: Some("sigil-client".to_owned()),
                scopes: vec!["files:read".to_owned()],
            }),
            client_capabilities: Default::default(),
        }),
        ..McpServerConfig::default()
    }
}

fn service(
    responses: Vec<McpOAuthHttpResponse>,
) -> (
    McpOAuthRuntimeService,
    Arc<MemoryCredentialStore>,
    Arc<QueueExecutor>,
) {
    let store = Arc::new(MemoryCredentialStore::default());
    let manager = Arc::new(McpOAuthCredentialManager::new_with_locator(
        store.clone(),
        store.clone(),
    ));
    let executor = QueueExecutor::new(responses);
    (
        McpOAuthRuntimeService::new(manager, executor.clone()),
        store,
        executor,
    )
}

#[tokio::test]
async fn manual_callback_sign_in_persists_exact_keyring_record_then_clear_is_explicit() {
    let mut responses = discovery_responses();
    responses.push(response(
        200,
        serde_json::json!({
            "access_token": "access-token-canary",
            "refresh_token": "refresh-token-canary",
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "files:read"
        }),
    ));
    responses.push(response(200, serde_json::json!({})));
    let (service, store, executor) = service(responses);
    let server = oauth_server();

    let initial = service.inspect(&server).await.expect("initial status");
    assert_eq!(initial.phase, McpOAuthAuthPhase::AuthenticationRequired);
    let flow = service.begin(&server).await.expect("begin OAuth");
    let prompt = flow.prompt();
    assert_eq!(prompt.phase, McpOAuthAuthPhase::AwaitingCallback);
    let authorization_url = prompt.authorization_url().expect("authorization URL");
    assert!(!format!("{prompt:?}").contains("auth.example/authorize"));
    let parsed = Url::parse(authorization_url.expose_secret()).expect("authorization URL");
    let state = parsed
        .query_pairs()
        .find_map(|(name, value)| (name == "state").then(|| value.into_owned()))
        .expect("state");
    let redirect = parsed
        .query_pairs()
        .find_map(|(name, value)| (name == "redirect_uri").then(|| value.into_owned()))
        .expect("redirect URI");
    let (control_tx, control_rx) = mpsc::channel(1);
    control_tx
        .send(McpOAuthFlowControl::ManualCallback(SecretString::new(
            format!("{redirect}?code=code-canary&state={state}"),
        )))
        .await
        .expect("manual callback");
    let signed_in = flow.run(control_rx).await.expect("complete OAuth");
    assert_eq!(signed_in.phase, McpOAuthAuthPhase::SignedIn);
    assert_eq!(
        executor.purposes(),
        [
            McpOAuthHttpPurpose::ProtectedResourceMetadata,
            McpOAuthHttpPurpose::AuthorizationServerMetadata,
            McpOAuthHttpPurpose::TokenExchange,
        ]
    );
    assert!(store.record.lock().expect("credential lock").is_some());
    assert_eq!(
        service.inspect(&server).await.expect("stored status").phase,
        McpOAuthAuthPhase::SignedIn
    );

    let (revoked, outcome) = service.revoke(&server).await.expect("revoke");
    assert_eq!(revoked.phase, McpOAuthAuthPhase::RevokedLocallyRetained);
    assert_eq!(outcome, McpOAuthRevocationOutcome::Revoked);
    assert!(store.record.lock().expect("credential lock").is_some());
    let cleared = service.clear_local(&server).await.expect("clear local");
    assert_eq!(cleared.phase, McpOAuthAuthPhase::AuthenticationRequired);
    assert!(store.record.lock().expect("credential lock").is_none());
    assert!(store.locator.lock().expect("locator lock").is_none());
}

#[tokio::test]
async fn cancellation_is_terminal_and_never_exchanges_or_persists_a_token() {
    let (service, store, executor) = service(discovery_responses());
    let flow = service.begin(&oauth_server()).await.expect("begin OAuth");
    let (control_tx, control_rx) = mpsc::channel(1);
    control_tx
        .send(McpOAuthFlowControl::Cancel)
        .await
        .expect("cancel");
    assert!(matches!(
        flow.run(control_rx).await,
        Err(McpOAuthFlowError::Cancelled)
    ));
    assert_eq!(executor.purposes().len(), 2);
    assert!(store.record.lock().expect("credential lock").is_none());
    assert!(store.locator.lock().expect("locator lock").is_none());
}

#[tokio::test]
async fn loopback_callback_completes_and_restart_recovers_exact_credential() {
    let mut responses = discovery_responses();
    responses.push(response(
        200,
        serde_json::json!({
            "access_token": "restart-access-token-canary",
            "refresh_token": "restart-refresh-token-canary",
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "files:read"
        }),
    ));
    let (service, store, executor) = service(responses);
    let server = oauth_server();
    let flow = service.begin(&server).await.expect("begin loopback OAuth");
    let authorization_url = flow
        .prompt()
        .authorization_url()
        .expect("authorization URL");
    let authorization_url =
        Url::parse(authorization_url.expose_secret()).expect("authorization URL");
    let state = authorization_url
        .query_pairs()
        .find_map(|(name, value)| (name == "state").then(|| value.into_owned()))
        .expect("state");
    let redirect_uri = authorization_url
        .query_pairs()
        .find_map(|(name, value)| (name == "redirect_uri").then(|| value.into_owned()))
        .expect("redirect URI");
    let redirect_uri = Url::parse(&redirect_uri).expect("loopback redirect URI");
    let address = format!(
        "{}:{}",
        redirect_uri.host_str().expect("loopback host"),
        redirect_uri.port().expect("loopback port")
    );
    let (_control_tx, control_rx) = mpsc::channel(1);
    let flow_task = tokio::spawn(flow.run(control_rx));
    let mut client = TcpStream::connect(address)
        .await
        .expect("connect loopback callback");
    let target = format!("{}?code=loopback-code&state={state}", redirect_uri.path());
    client
        .write_all(
            format!(
                "GET {target} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                redirect_uri.host_str().expect("loopback host")
            )
            .as_bytes(),
        )
        .await
        .expect("write loopback callback");
    let mut callback_response = Vec::new();
    client
        .read_to_end(&mut callback_response)
        .await
        .expect("read loopback response");
    assert!(callback_response.starts_with(b"HTTP/1.1 200"));
    let signed_in = flow_task
        .await
        .expect("join loopback flow")
        .expect("complete loopback flow");
    assert_eq!(signed_in.phase, McpOAuthAuthPhase::SignedIn);

    let restarted = McpOAuthRuntimeService::new(
        Arc::new(McpOAuthCredentialManager::new_with_locator(
            store.clone(),
            store.clone(),
        )),
        executor,
    );
    let recovered = restarted.inspect(&server).await.expect("restart inspect");
    assert_eq!(recovered.phase, McpOAuthAuthPhase::SignedIn);
    assert_eq!(recovered.credential, McpOAuthCredentialStatus::Present);
    assert!(!format!("{recovered:?}").contains("restart-access-token-canary"));
    restarted
        .clear_local(&server)
        .await
        .expect("restart cleanup");
}

#[test]
fn protocol_failure_classes_project_to_actionable_runtime_codes() {
    for (protocol, expected) in [
        (
            McpOAuthProtocolError::DestinationRejected,
            McpOAuthAuthErrorCode::DestinationRejected,
        ),
        (
            McpOAuthProtocolError::BudgetExhausted,
            McpOAuthAuthErrorCode::BudgetExhausted,
        ),
        (
            McpOAuthProtocolError::Transport,
            McpOAuthAuthErrorCode::Transport,
        ),
    ] {
        assert_eq!(McpOAuthFlowError::Protocol(protocol).code(), expected);
    }
    for (credential, expected) in [
        (
            McpOAuthCredentialError::DestinationRejected,
            McpOAuthAuthErrorCode::DestinationRejected,
        ),
        (
            McpOAuthCredentialError::BudgetExhausted,
            McpOAuthAuthErrorCode::BudgetExhausted,
        ),
        (
            McpOAuthCredentialError::Transport,
            McpOAuthAuthErrorCode::Transport,
        ),
    ] {
        assert_eq!(McpOAuthFlowError::Credential(credential).code(), expected);
    }
}
