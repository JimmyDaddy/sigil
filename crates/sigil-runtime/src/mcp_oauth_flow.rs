use std::sync::Arc;

use sigil_kernel::{McpServerConfig, SecretString};
use sigil_mcp::{
    McpOAuthChallenge, McpOAuthClientIntent, McpOAuthCredentialError, McpOAuthCredentialLookup,
    McpOAuthCredentialRecord, McpOAuthCredentialScope, McpOAuthCredentialStatus,
    McpOAuthHttpExecutor, McpOAuthLoopbackListener, McpOAuthPendingAuthorization,
    McpOAuthProtocolError, McpOAuthRevocationOutcome, discover_oauth_authorization_server,
    exchange_oauth_authorization_code, prepare_oauth_client,
};
use thiserror::Error;
use tokio::sync::mpsc;
use url::Url;

use crate::McpOAuthCredentialManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpOAuthAuthPhase {
    NotConfigured,
    AuthenticationRequired,
    Discovering,
    AwaitingCallback,
    ExchangingCode,
    SignedIn,
    Refreshing,
    Revoking,
    RevokedLocallyRetained,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpOAuthAuthErrorCode {
    ConfigurationInvalid,
    DestinationRejected,
    MetadataUnavailable,
    AuthorizationRejected,
    CallbackInvalid,
    TokenRejected,
    CredentialStoreUnavailable,
    CredentialStoreRejected,
    RefreshRejected,
    RevocationRejected,
    BudgetExhausted,
    Transport,
}

#[derive(Clone)]
pub struct McpOAuthAuthStatus {
    pub server_name: String,
    pub phase: McpOAuthAuthPhase,
    pub resource: String,
    pub issuer: Option<String>,
    pub scopes: Vec<String>,
    pub credential: McpOAuthCredentialStatus,
    pub error: Option<McpOAuthAuthErrorCode>,
    authorization_url: Option<SecretString>,
}

impl std::fmt::Debug for McpOAuthAuthStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpOAuthAuthStatus")
            .field("server_name", &self.server_name)
            .field("phase", &self.phase)
            .field("resource", &self.resource)
            .field("issuer", &self.issuer)
            .field("scope_count", &self.scopes.len())
            .field("credential", &self.credential)
            .field("error", &self.error)
            .field("has_authorization_url", &self.authorization_url.is_some())
            .finish()
    }
}

impl McpOAuthAuthStatus {
    #[must_use]
    pub fn authorization_url(&self) -> Option<SecretString> {
        self.authorization_url.clone()
    }

    #[must_use]
    pub fn with_phase(mut self, phase: McpOAuthAuthPhase) -> Self {
        self.phase = phase;
        if phase != McpOAuthAuthPhase::AwaitingCallback {
            self.authorization_url = None;
        }
        self
    }

    #[must_use]
    pub fn failed(mut self, error: &McpOAuthFlowError) -> Self {
        self.phase = McpOAuthAuthPhase::Failed;
        self.error = Some(error.code());
        self.authorization_url = None;
        self
    }

    #[must_use]
    pub fn cancelled(mut self) -> Self {
        self.phase = McpOAuthAuthPhase::Cancelled;
        self.authorization_url = None;
        self
    }
}

#[derive(Debug)]
pub enum McpOAuthFlowControl {
    ManualCallback(SecretString),
    Cancel,
}

#[derive(Debug, Error)]
pub enum McpOAuthFlowError {
    #[error("remote MCP OAuth configuration is invalid")]
    Configuration,
    #[error("remote MCP OAuth protocol failed")]
    Protocol(#[from] McpOAuthProtocolError),
    #[error("remote MCP OAuth credential operation failed")]
    Credential(#[from] McpOAuthCredentialError),
    #[error("remote MCP OAuth flow was cancelled")]
    Cancelled,
}

impl McpOAuthFlowError {
    #[must_use]
    pub fn code(&self) -> McpOAuthAuthErrorCode {
        match self {
            Self::Configuration => McpOAuthAuthErrorCode::ConfigurationInvalid,
            Self::Cancelled => McpOAuthAuthErrorCode::CallbackInvalid,
            Self::Protocol(error) => match error {
                McpOAuthProtocolError::InvalidResource
                | McpOAuthProtocolError::InvalidChallenge
                | McpOAuthProtocolError::InvalidClient => {
                    McpOAuthAuthErrorCode::ConfigurationInvalid
                }
                McpOAuthProtocolError::MetadataUnavailable
                | McpOAuthProtocolError::InvalidMetadata
                | McpOAuthProtocolError::AmbiguousAuthorizationServer
                | McpOAuthProtocolError::UnsupportedAuthorizationServer => {
                    McpOAuthAuthErrorCode::MetadataUnavailable
                }
                McpOAuthProtocolError::AuthorizationRejected => {
                    McpOAuthAuthErrorCode::AuthorizationRejected
                }
                McpOAuthProtocolError::InvalidAuthorizationResponse
                | McpOAuthProtocolError::FlowExpired
                | McpOAuthProtocolError::FlowConsumed
                | McpOAuthProtocolError::CallbackFailed => McpOAuthAuthErrorCode::CallbackInvalid,
                McpOAuthProtocolError::ClientRegistrationRejected
                | McpOAuthProtocolError::TokenRejected => McpOAuthAuthErrorCode::TokenRejected,
                McpOAuthProtocolError::DestinationRejected => {
                    McpOAuthAuthErrorCode::DestinationRejected
                }
                McpOAuthProtocolError::BudgetExhausted => McpOAuthAuthErrorCode::BudgetExhausted,
                McpOAuthProtocolError::Transport => McpOAuthAuthErrorCode::Transport,
            },
            Self::Credential(error) => match error {
                McpOAuthCredentialError::StoreUnavailable => {
                    McpOAuthAuthErrorCode::CredentialStoreUnavailable
                }
                McpOAuthCredentialError::StoreRejected => {
                    McpOAuthAuthErrorCode::CredentialStoreRejected
                }
                McpOAuthCredentialError::InvalidRefresh
                | McpOAuthCredentialError::RefreshRejected
                | McpOAuthCredentialError::AuthenticationRequired => {
                    McpOAuthAuthErrorCode::RefreshRejected
                }
                McpOAuthCredentialError::RevocationRejected => {
                    McpOAuthAuthErrorCode::RevocationRejected
                }
                McpOAuthCredentialError::DestinationRejected => {
                    McpOAuthAuthErrorCode::DestinationRejected
                }
                McpOAuthCredentialError::BudgetExhausted => McpOAuthAuthErrorCode::BudgetExhausted,
                McpOAuthCredentialError::Transport => McpOAuthAuthErrorCode::Transport,
                McpOAuthCredentialError::InvalidScope | McpOAuthCredentialError::InvalidRecord => {
                    McpOAuthAuthErrorCode::ConfigurationInvalid
                }
            },
        }
    }
}

pub struct McpOAuthRuntimeService {
    manager: Arc<McpOAuthCredentialManager>,
    executor: Arc<dyn McpOAuthHttpExecutor>,
}

impl std::fmt::Debug for McpOAuthRuntimeService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpOAuthRuntimeService")
            .finish_non_exhaustive()
    }
}

impl McpOAuthRuntimeService {
    #[must_use]
    pub fn new(
        manager: Arc<McpOAuthCredentialManager>,
        executor: Arc<dyn McpOAuthHttpExecutor>,
    ) -> Self {
        Self { manager, executor }
    }

    pub async fn inspect(
        &self,
        server: &McpServerConfig,
    ) -> Result<McpOAuthAuthStatus, McpOAuthFlowError> {
        let Some((lookup, configured_scopes)) = oauth_lookup(server)? else {
            return Ok(status(
                server,
                McpOAuthAuthPhase::NotConfigured,
                None,
                Vec::new(),
                McpOAuthCredentialStatus::Missing,
                None,
                None,
            ));
        };
        match self
            .manager
            .status_for_lookup(&lookup, now_epoch_secs())
            .await
        {
            Ok(Some((scope, credential))) => Ok(status(
                server,
                if matches!(
                    credential,
                    McpOAuthCredentialStatus::Present | McpOAuthCredentialStatus::Expiring
                ) {
                    McpOAuthAuthPhase::SignedIn
                } else {
                    McpOAuthAuthPhase::AuthenticationRequired
                },
                Some(scope.issuer()),
                scope.scopes().to_vec(),
                credential,
                None,
                None,
            )),
            Ok(None) => Ok(status(
                server,
                McpOAuthAuthPhase::AuthenticationRequired,
                None,
                configured_scopes,
                McpOAuthCredentialStatus::Missing,
                None,
                None,
            )),
            Err(error) => Ok(status(
                server,
                McpOAuthAuthPhase::Failed,
                None,
                configured_scopes,
                McpOAuthCredentialStatus::Unavailable,
                Some(McpOAuthFlowError::Credential(error).code()),
                None,
            )),
        }
    }

    pub async fn begin(
        &self,
        server: &McpServerConfig,
    ) -> Result<McpOAuthPreparedFlow, McpOAuthFlowError> {
        let remote = server
            .streamable_http()
            .ok_or(McpOAuthFlowError::Configuration)?;
        let oauth = remote
            .oauth
            .as_ref()
            .ok_or(McpOAuthFlowError::Configuration)?;
        let lookup = McpOAuthCredentialLookup::new(
            server.name.clone(),
            remote.url.clone(),
            oauth.client_id.clone(),
            oauth.scopes.clone(),
        )?;
        let challenge = McpOAuthChallenge::parse("Bearer", SecretString::new(remote.url.clone()))?
            .ok_or(McpOAuthFlowError::Configuration)?;
        let listener = McpOAuthLoopbackListener::bind().await?;
        let discovery =
            discover_oauth_authorization_server(self.executor.as_ref(), &challenge).await?;
        let intent = McpOAuthClientIntent::new(oauth.client_id.clone(), oauth.scopes.clone())?;
        let client = prepare_oauth_client(
            self.executor.as_ref(),
            &discovery,
            &intent,
            listener.redirect_uri(),
        )
        .await?;
        let scopes = discovery.requested_scopes(&intent)?;
        let exact_scope = McpOAuthCredentialScope::from_authorization(
            server.name.clone(),
            &discovery,
            &client,
            scopes.clone(),
        )?;
        let pending = McpOAuthPendingAuthorization::new(
            discovery.clone(),
            client.clone(),
            scopes.clone(),
            listener.redirect_uri(),
        )?;
        let authorization_url = pending.authorization_url();
        let prompt = status(
            server,
            McpOAuthAuthPhase::AwaitingCallback,
            Some(discovery.issuer()),
            scopes,
            McpOAuthCredentialStatus::Missing,
            None,
            Some(authorization_url),
        );
        Ok(McpOAuthPreparedFlow {
            server: server.clone(),
            manager: Arc::clone(&self.manager),
            executor: Arc::clone(&self.executor),
            lookup,
            exact_scope,
            discovery,
            client,
            listener: Some(listener),
            pending,
            prompt,
        })
    }

    pub async fn refresh(
        &self,
        server: &McpServerConfig,
        static_header_fingerprint: &str,
    ) -> Result<McpOAuthAuthStatus, McpOAuthFlowError> {
        let (lookup, _) = oauth_lookup(server)?.ok_or(McpOAuthFlowError::Configuration)?;
        let record = self
            .manager
            .load_for_lookup(&lookup)
            .await?
            .ok_or(McpOAuthCredentialError::AuthenticationRequired)?;
        self.manager
            .refresh_now(
                record.scope(),
                static_header_fingerprint,
                now_epoch_secs(),
                self.executor.as_ref(),
            )
            .await?;
        self.inspect(server).await
    }

    pub async fn revoke(
        &self,
        server: &McpServerConfig,
    ) -> Result<(McpOAuthAuthStatus, McpOAuthRevocationOutcome), McpOAuthFlowError> {
        let (lookup, _) = oauth_lookup(server)?.ok_or(McpOAuthFlowError::Configuration)?;
        let record = self
            .manager
            .load_for_lookup(&lookup)
            .await?
            .ok_or(McpOAuthCredentialError::AuthenticationRequired)?;
        let outcome = self
            .manager
            .revoke(record.scope(), self.executor.as_ref())
            .await?;
        Ok((
            status(
                server,
                McpOAuthAuthPhase::RevokedLocallyRetained,
                Some(record.scope().issuer()),
                record.scope().scopes().to_vec(),
                record.status(now_epoch_secs()),
                None,
                None,
            ),
            outcome,
        ))
    }

    pub async fn clear_local(
        &self,
        server: &McpServerConfig,
    ) -> Result<McpOAuthAuthStatus, McpOAuthFlowError> {
        let (lookup, scopes) = oauth_lookup(server)?.ok_or(McpOAuthFlowError::Configuration)?;
        self.manager.clear_local_for_lookup(&lookup).await?;
        Ok(status(
            server,
            McpOAuthAuthPhase::AuthenticationRequired,
            None,
            scopes,
            McpOAuthCredentialStatus::Missing,
            None,
            None,
        ))
    }
}

pub struct McpOAuthPreparedFlow {
    server: McpServerConfig,
    manager: Arc<McpOAuthCredentialManager>,
    executor: Arc<dyn McpOAuthHttpExecutor>,
    lookup: McpOAuthCredentialLookup,
    exact_scope: McpOAuthCredentialScope,
    discovery: sigil_mcp::McpOAuthDiscovery,
    client: sigil_mcp::McpOAuthClientRegistration,
    listener: Option<McpOAuthLoopbackListener>,
    pending: McpOAuthPendingAuthorization,
    prompt: McpOAuthAuthStatus,
}

impl std::fmt::Debug for McpOAuthPreparedFlow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpOAuthPreparedFlow")
            .field("server_name", &self.server.name)
            .field("lookup", &self.lookup)
            .finish_non_exhaustive()
    }
}

impl McpOAuthPreparedFlow {
    #[must_use]
    pub fn prompt(&self) -> McpOAuthAuthStatus {
        self.prompt.clone()
    }

    pub async fn run(
        mut self,
        mut controls: mpsc::Receiver<McpOAuthFlowControl>,
    ) -> Result<McpOAuthAuthStatus, McpOAuthFlowError> {
        let listener = self
            .listener
            .take()
            .ok_or(McpOAuthFlowError::Configuration)?;
        let callback = tokio::select! {
            accepted = listener.receive_callback() => {
                let accepted = accepted?;
                accepted.complete(&mut self.pending).await?
            }
            control = controls.recv() => {
                match control {
                    Some(McpOAuthFlowControl::ManualCallback(callback)) => {
                        self.pending.complete_callback(callback)?
                    }
                    Some(McpOAuthFlowControl::Cancel) | None => {
                        return Err(McpOAuthFlowError::Cancelled);
                    }
                }
            }
        };
        let token = exchange_oauth_authorization_code(self.executor.as_ref(), callback).await?;
        let record = McpOAuthCredentialRecord::from_token_response(
            self.exact_scope,
            &self.discovery,
            &self.client,
            &token,
            now_epoch_secs(),
        )?;
        self.manager
            .persist_for_lookup(&self.lookup, &record)
            .await?;
        Ok(status(
            &self.server,
            McpOAuthAuthPhase::SignedIn,
            Some(record.scope().issuer()),
            record.scope().scopes().to_vec(),
            record.status(now_epoch_secs()),
            None,
            None,
        ))
    }
}

fn oauth_lookup(
    server: &McpServerConfig,
) -> Result<Option<(McpOAuthCredentialLookup, Vec<String>)>, McpOAuthFlowError> {
    let Some(remote) = server.streamable_http() else {
        return Ok(None);
    };
    let Some(oauth) = remote.oauth.as_ref() else {
        return Ok(None);
    };
    Ok(Some((
        McpOAuthCredentialLookup::new(
            server.name.clone(),
            remote.url.clone(),
            oauth.client_id.clone(),
            oauth.scopes.clone(),
        )?,
        oauth.scopes.clone(),
    )))
}

fn status(
    server: &McpServerConfig,
    phase: McpOAuthAuthPhase,
    issuer: Option<&str>,
    scopes: Vec<String>,
    credential: McpOAuthCredentialStatus,
    error: Option<McpOAuthAuthErrorCode>,
    authorization_url: Option<SecretString>,
) -> McpOAuthAuthStatus {
    McpOAuthAuthStatus {
        server_name: server.name.clone(),
        phase,
        resource: server
            .streamable_http()
            .map(|remote| safe_resource(&remote.url))
            .unwrap_or_else(|| "not configured".to_owned()),
        issuer: issuer.map(safe_resource),
        scopes,
        credential,
        error,
        authorization_url,
    }
}

fn safe_resource(value: &str) -> String {
    Url::parse(value)
        .ok()
        .and_then(|url| {
            let host = url.host_str()?.to_owned();
            let port = url.port_or_known_default()?;
            Some(format!("{}://{host}:{port}{}", url.scheme(), url.path()))
        })
        .unwrap_or_else(|| "invalid".to_owned())
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
#[path = "tests/mcp_oauth_flow_tests.rs"]
mod tests;
