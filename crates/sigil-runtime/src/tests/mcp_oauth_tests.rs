use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use sigil_kernel::SecretString;
use sigil_mcp::{
    McpOAuthChallenge, McpOAuthClientIntent, McpOAuthCredentialError, McpOAuthCredentialRecord,
    McpOAuthCredentialScope, McpOAuthCredentialStatus, McpOAuthCredentialStore,
    McpOAuthHttpExecutor, McpOAuthHttpPurpose, McpOAuthHttpRequest, McpOAuthHttpResponse,
    McpOAuthPendingAuthorization, McpOAuthRevocationOutcome, McpOAuthTransportError,
    discover_oauth_authorization_server, exchange_oauth_authorization_code, prepare_oauth_client,
};
use url::Url;

use super::*;

#[derive(Default)]
struct MemoryStore {
    record: StdMutex<Option<McpOAuthCredentialRecord>>,
    unavailable: AtomicBool,
    stores: AtomicUsize,
    deletes: AtomicUsize,
}

impl MemoryStore {
    fn with_record(record: McpOAuthCredentialRecord) -> Arc<Self> {
        Arc::new(Self {
            record: StdMutex::new(Some(record)),
            ..Default::default()
        })
    }

    fn loaded(&self) -> Option<McpOAuthCredentialRecord> {
        self.record.lock().expect("record lock").clone()
    }
}

#[async_trait]
impl McpOAuthCredentialStore for MemoryStore {
    async fn load(
        &self,
        _scope: &McpOAuthCredentialScope,
    ) -> Result<Option<McpOAuthCredentialRecord>, McpOAuthCredentialError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(McpOAuthCredentialError::StoreUnavailable);
        }
        Ok(self.loaded())
    }

    async fn store(
        &self,
        record: &McpOAuthCredentialRecord,
    ) -> Result<(), McpOAuthCredentialError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(McpOAuthCredentialError::StoreUnavailable);
        }
        self.stores.fetch_add(1, Ordering::SeqCst);
        *self.record.lock().expect("record lock") = Some(record.clone());
        Ok(())
    }

    async fn delete(
        &self,
        _scope: &McpOAuthCredentialScope,
    ) -> Result<bool, McpOAuthCredentialError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(McpOAuthCredentialError::StoreUnavailable);
        }
        self.deletes.fetch_add(1, Ordering::SeqCst);
        Ok(self.record.lock().expect("record lock").take().is_some())
    }
}

struct QueueExecutor {
    responses: StdMutex<VecDeque<Result<McpOAuthHttpResponse, McpOAuthTransportError>>>,
    requests: StdMutex<Vec<(McpOAuthHttpPurpose, String, String)>>,
    delay: Duration,
}

impl QueueExecutor {
    fn new(
        responses: Vec<Result<McpOAuthHttpResponse, McpOAuthTransportError>>,
        delay: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            responses: StdMutex::new(responses.into()),
            requests: StdMutex::new(Vec::new()),
            delay,
        })
    }

    fn request_count(&self) -> usize {
        self.requests.lock().expect("requests lock").len()
    }
}

#[async_trait]
impl McpOAuthHttpExecutor for QueueExecutor {
    async fn execute(
        &self,
        request: McpOAuthHttpRequest,
    ) -> Result<McpOAuthHttpResponse, McpOAuthTransportError> {
        self.requests.lock().expect("requests lock").push((
            request.purpose(),
            request.destination().to_owned(),
            request.body().unwrap_or_default().to_owned(),
        ));
        let response = self
            .responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .unwrap_or(Err(McpOAuthTransportError::Transport));
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        response
    }
}

fn json_response(status: u16, value: serde_json::Value) -> McpOAuthHttpResponse {
    McpOAuthHttpResponse::new(
        status,
        Some("application/json".to_owned()),
        SecretString::new(value.to_string()),
    )
}

async fn authorized_record(
    now_epoch_secs: u64,
    expires_in_secs: u64,
) -> (McpOAuthCredentialScope, McpOAuthCredentialRecord) {
    let discovery_executor = QueueExecutor::new(
        vec![
            Ok(json_response(
                200,
                serde_json::json!({
                    "resource": "https://mcp.example/public/mcp",
                    "authorization_servers": ["https://auth.example/tenant"],
                    "scopes_supported": ["files:read"]
                }),
            )),
            Ok(json_response(
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
            )),
        ],
        Duration::ZERO,
    );
    let challenge = McpOAuthChallenge::parse(
        "Bearer scope=\"files:read\"",
        SecretString::new("https://mcp.example/public/mcp"),
    )
    .expect("challenge")
    .expect("Bearer");
    let discovery = discover_oauth_authorization_server(discovery_executor.as_ref(), &challenge)
        .await
        .expect("discovery");
    let intent = McpOAuthClientIntent::new(
        Some("sigil-client".to_owned()),
        vec!["files:read".to_owned()],
    )
    .expect("intent");
    let client = prepare_oauth_client(
        discovery_executor.as_ref(),
        &discovery,
        &intent,
        "http://127.0.0.1:43123/callback",
    )
    .await
    .expect("client");
    let mut pending = McpOAuthPendingAuthorization::new(
        discovery.clone(),
        client.clone(),
        vec!["files:read".to_owned()],
        "http://127.0.0.1:43123/callback",
    )
    .expect("pending");
    let state = Url::parse(pending.authorization_url().expose_secret())
        .expect("authorization URL")
        .query_pairs()
        .find_map(|(name, value)| (name == "state").then(|| value.into_owned()))
        .expect("state");
    let code = pending
        .complete_callback(SecretString::new(format!(
            "http://127.0.0.1:43123/callback?code=code-canary&state={state}"
        )))
        .expect("callback");
    let token_executor = QueueExecutor::new(
        vec![Ok(json_response(
            200,
            serde_json::json!({
                "access_token": "access-token-canary",
                "refresh_token": "refresh-token-canary",
                "token_type": "Bearer",
                "expires_in": expires_in_secs,
                "scope": "files:read"
            }),
        ))],
        Duration::ZERO,
    );
    let token = exchange_oauth_authorization_code(token_executor.as_ref(), code)
        .await
        .expect("token");
    let scope = McpOAuthCredentialScope::from_authorization(
        "github",
        &discovery,
        &client,
        vec!["files:read".to_owned()],
    )
    .expect("scope");
    let record = McpOAuthCredentialRecord::from_token_response(
        scope.clone(),
        &discovery,
        &client,
        &token,
        now_epoch_secs,
    )
    .expect("record");
    (scope, record)
}

#[tokio::test]
async fn unavailable_store_is_not_projected_as_missing_or_fallback() {
    let (scope, _) = authorized_record(1_000, 3_600).await;
    let store = Arc::new(MemoryStore::default());
    store.unavailable.store(true, Ordering::SeqCst);
    let manager = McpOAuthCredentialManager::new(store);
    let executor = QueueExecutor::new(Vec::new(), Duration::ZERO);

    assert_eq!(
        manager.status(&scope, 1_000).await,
        McpOAuthCredentialStatus::Unavailable
    );
    assert!(matches!(
        manager
            .bearer_snapshot(&scope, "static-fingerprint", 1_000, executor.as_ref())
            .await,
        Err(McpOAuthCredentialError::StoreUnavailable)
    ));
    assert_eq!(executor.request_count(), 0);
}

#[tokio::test]
async fn expiring_requests_share_one_refresh_and_atomic_rotated_snapshot() {
    let (scope, record) = authorized_record(1_000, 1).await;
    let store = MemoryStore::with_record(record);
    let manager = Arc::new(McpOAuthCredentialManager::new(store.clone()));
    let executor = QueueExecutor::new(
        vec![Ok(json_response(
            200,
            serde_json::json!({
                "access_token": "rotated-access-canary",
                "refresh_token": "rotated-refresh-canary",
                "token_type": "Bearer",
                "expires_in": 3600,
                "scope": "files:read"
            }),
        ))],
        Duration::from_millis(25),
    );
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let manager = Arc::clone(&manager);
        let executor = Arc::clone(&executor);
        let scope = scope.clone();
        tasks.push(tokio::spawn(async move {
            manager
                .bearer_snapshot(&scope, "static-fingerprint", 1_002, executor.as_ref())
                .await
                .expect("snapshot")
                .live_fingerprint()
                .to_owned()
        }));
    }
    let mut fingerprints = Vec::new();
    for task in tasks {
        fingerprints.push(task.await.expect("join"));
    }
    assert_eq!(executor.request_count(), 1);
    assert_eq!(store.stores.load(Ordering::SeqCst), 1);
    assert!(fingerprints.windows(2).all(|values| values[0] == values[1]));
    let stored = store.loaded().expect("record");
    assert_eq!(stored.status(1_002), McpOAuthCredentialStatus::Present);
    let debug = format!("{stored:?}");
    assert!(!debug.contains("rotated-access-canary"));
    assert!(!debug.contains("rotated-refresh-canary"));
}

#[tokio::test]
async fn invalid_refresh_is_persistently_disabled_and_never_loops() {
    let (scope, record) = authorized_record(1_000, 1).await;
    let store = MemoryStore::with_record(record);
    let manager = McpOAuthCredentialManager::new(store.clone());
    let executor = QueueExecutor::new(
        vec![Ok(json_response(
            400,
            serde_json::json!({"error": "invalid_grant"}),
        ))],
        Duration::ZERO,
    );

    assert!(matches!(
        manager
            .bearer_snapshot(&scope, "static-fingerprint", 1_002, executor.as_ref())
            .await,
        Err(McpOAuthCredentialError::AuthenticationRequired)
    ));
    assert_eq!(executor.request_count(), 1);
    assert!(!store.loaded().expect("record").has_refresh_token());
    assert!(matches!(
        manager
            .bearer_snapshot(&scope, "static-fingerprint", 1_003, executor.as_ref())
            .await,
        Err(McpOAuthCredentialError::AuthenticationRequired)
    ));
    assert_eq!(executor.request_count(), 1);
}

#[tokio::test]
async fn transport_ambiguity_and_unauthorized_never_retry_in_place() {
    let (scope, record) = authorized_record(1_000, 1).await;
    let store = MemoryStore::with_record(record);
    let manager = McpOAuthCredentialManager::new(store.clone());
    let executor = QueueExecutor::new(vec![Err(McpOAuthTransportError::Transport)], Duration::ZERO);
    assert!(matches!(
        manager
            .bearer_snapshot(&scope, "static-fingerprint", 1_002, executor.as_ref())
            .await,
        Err(McpOAuthCredentialError::Transport)
    ));
    assert_eq!(executor.request_count(), 1);

    manager
        .mark_unauthorized(&scope, 1_003)
        .await
        .expect("mark unauthorized");
    let marked = store.loaded().expect("record");
    assert_eq!(marked.status(1_003), McpOAuthCredentialStatus::Expired);
    assert!(marked.has_refresh_token());
    assert_eq!(executor.request_count(), 1);
}

#[tokio::test]
async fn revoke_failure_keeps_local_record_until_separate_clear() {
    let (scope, record) = authorized_record(1_000, 3_600).await;
    let store = MemoryStore::with_record(record);
    let manager = McpOAuthCredentialManager::new(store.clone());
    let rejected = QueueExecutor::new(
        vec![Ok(McpOAuthHttpResponse::new(
            503,
            None,
            SecretString::new(String::new()),
        ))],
        Duration::ZERO,
    );
    assert!(matches!(
        manager.revoke(&scope, rejected.as_ref()).await,
        Err(McpOAuthCredentialError::RevocationRejected)
    ));
    assert!(store.loaded().is_some());
    assert_eq!(store.deletes.load(Ordering::SeqCst), 0);

    let accepted = QueueExecutor::new(
        vec![Ok(McpOAuthHttpResponse::new(
            200,
            None,
            SecretString::new(String::new()),
        ))],
        Duration::ZERO,
    );
    assert_eq!(
        manager
            .revoke(&scope, accepted.as_ref())
            .await
            .expect("revoke"),
        McpOAuthRevocationOutcome::Revoked
    );
    assert!(store.loaded().is_some());

    assert!(manager.clear_local(&scope).await.expect("clear"));
    assert!(store.loaded().is_none());
    assert_eq!(
        manager.status(&scope, 1_001).await,
        McpOAuthCredentialStatus::Missing
    );
}
