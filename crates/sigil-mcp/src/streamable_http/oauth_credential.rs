use std::{fmt, sync::OnceLock};

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigil_kernel::SecretString;
use thiserror::Error;
use url::Url;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use super::oauth::{
    McpOAuthClientRegistration, McpOAuthDiscovery, McpOAuthHttpExecutor, McpOAuthHttpPurpose,
    McpOAuthHttpRequest, McpOAuthHttpResponse, McpOAuthProtocolError, McpOAuthTokenResponse,
    McpOAuthTransportError, basic_client_authorization, parse_bearer_token_response,
};

const CREDENTIAL_RECORD_VERSION: u8 = 1;
const CREDENTIAL_LOCATOR_VERSION: u8 = 1;
const CREDENTIAL_SERVICE: &str = "io.github.sigil.mcp-oauth.v1";
// Windows Credential Manager is the narrowest supported native store.
const MAX_CREDENTIAL_RECORD_BYTES: usize = 2_560;
const MAX_SERVER_NAME_BYTES: usize = 256;
const MAX_BINDING_TEXT_BYTES: usize = 8 * 1024;
const MAX_SECRET_BYTES: usize = 64 * 1024;
const MAX_SCOPES: usize = 32;
const MAX_SCOPE_BYTES: usize = 256;
const MAX_SCOPE_TOTAL_BYTES: usize = 4 * 1024;
const EXPIRY_SKEW_SECS: u64 = 60;

static CREDENTIAL_FINGERPRINT_KEY: OnceLock<[u8; 32]> = OnceLock::new();

/// Redaction-safe credential persistence, refresh and revocation failures.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum McpOAuthCredentialError {
    #[error("remote MCP OAuth credential scope is invalid")]
    InvalidScope,
    #[error("remote MCP OAuth credential record is invalid or exceeds limits")]
    InvalidRecord,
    #[error("system credential store is unavailable")]
    StoreUnavailable,
    #[error("system credential store rejected the OAuth credential update")]
    StoreRejected,
    #[error("remote MCP authentication is required")]
    AuthenticationRequired,
    #[error("remote MCP OAuth refresh token is invalid or expired")]
    InvalidRefresh,
    #[error("remote MCP OAuth refresh was rejected")]
    RefreshRejected,
    #[error("remote MCP OAuth revocation was rejected")]
    RevocationRejected,
    #[error("remote MCP OAuth destination authorization was rejected")]
    DestinationRejected,
    #[error("remote MCP OAuth network budget was exhausted")]
    BudgetExhausted,
    #[error("remote MCP OAuth transport failed")]
    Transport,
}

/// Public, secret-free projection of one credential scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpOAuthCredentialStatus {
    Missing,
    Present,
    Expiring,
    Expired,
    Unavailable,
}

/// Exact identity binding used as the system-keyring account scope.
#[derive(Clone, PartialEq, Eq)]
pub struct McpOAuthCredentialScope {
    server_name: String,
    resource: String,
    issuer: String,
    client_id: String,
    scopes: Vec<String>,
    binding_id: String,
}

/// Stable, secret-free lookup binding derived from the configured OAuth intent.
///
/// The exact issuer/client/scope credential binding remains inside the located keyring record.
#[derive(Clone, PartialEq, Eq)]
pub struct McpOAuthCredentialLookup {
    server_name: String,
    resource: String,
    configured_client_id: Option<String>,
    configured_scopes: Vec<String>,
    binding_id: String,
}

impl fmt::Debug for McpOAuthCredentialLookup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthCredentialLookup")
            .field("binding_id", &self.binding_id)
            .field("scope_count", &self.configured_scopes.len())
            .finish_non_exhaustive()
    }
}

impl McpOAuthCredentialLookup {
    pub fn new(
        server_name: impl Into<String>,
        resource: impl Into<String>,
        configured_client_id: Option<String>,
        configured_scopes: Vec<String>,
    ) -> Result<Self, McpOAuthCredentialError> {
        let server_name = server_name.into();
        let resource = resource.into();
        if !valid_text(&server_name, MAX_SERVER_NAME_BYTES)
            || !valid_https_binding(&resource)
            || configured_client_id.as_deref().is_some_and(|client_id| {
                !valid_text(client_id, MAX_BINDING_TEXT_BYTES)
                    || client_id.chars().any(char::is_whitespace)
            })
        {
            return Err(McpOAuthCredentialError::InvalidScope);
        }
        let configured_scopes = normalize_scopes(configured_scopes)?;
        let mut digest = Sha256::new();
        update_digest(&mut digest, "server", server_name.as_bytes());
        update_digest(&mut digest, "resource", resource.as_bytes());
        update_digest(
            &mut digest,
            "configured_client",
            configured_client_id.as_deref().unwrap_or("").as_bytes(),
        );
        for scope in &configured_scopes {
            update_digest(&mut digest, "configured_scope", scope.as_bytes());
        }
        Ok(Self {
            server_name,
            resource,
            configured_client_id,
            configured_scopes,
            binding_id: format!("sha256:{:x}", digest.finalize()),
        })
    }

    #[must_use]
    pub fn binding_id(&self) -> &str {
        &self.binding_id
    }

    fn keyring_account(&self) -> String {
        format!("lookup-{}", self.binding_id)
    }

    fn accepts(&self, scope: &McpOAuthCredentialScope) -> bool {
        scope.server_name() == self.server_name
            && scope.resource() == self.resource
            && self
                .configured_client_id
                .as_deref()
                .is_none_or(|client_id| scope.client_id() == client_id)
    }
}

impl fmt::Debug for McpOAuthCredentialScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthCredentialScope")
            .field("binding_id", &self.binding_id)
            .field("scope_count", &self.scopes.len())
            .finish_non_exhaustive()
    }
}

impl McpOAuthCredentialScope {
    /// Builds a normalized credential scope. Scope order does not affect its identity.
    pub fn new(
        server_name: impl Into<String>,
        resource: impl Into<String>,
        issuer: impl Into<String>,
        client_id: impl Into<String>,
        scopes: Vec<String>,
    ) -> Result<Self, McpOAuthCredentialError> {
        let server_name = server_name.into();
        let resource = resource.into();
        let issuer = issuer.into();
        let client_id = client_id.into();
        if !valid_text(&server_name, MAX_SERVER_NAME_BYTES)
            || !valid_text(&client_id, MAX_BINDING_TEXT_BYTES)
            || client_id.chars().any(char::is_whitespace)
            || !valid_https_binding(&resource)
            || !valid_issuer_binding(&issuer)
        {
            return Err(McpOAuthCredentialError::InvalidScope);
        }
        let scopes = normalize_scopes(scopes)?;
        let binding_id = scope_binding_id(&server_name, &resource, &issuer, &client_id, &scopes);
        Ok(Self {
            server_name,
            resource,
            issuer,
            client_id,
            scopes,
            binding_id,
        })
    }

    /// Builds the exact scope from a validated discovery and client selection.
    pub fn from_authorization(
        server_name: impl Into<String>,
        discovery: &McpOAuthDiscovery,
        client: &McpOAuthClientRegistration,
        scopes: Vec<String>,
    ) -> Result<Self, McpOAuthCredentialError> {
        Self::new(
            server_name,
            discovery.resource().as_str(),
            discovery.issuer(),
            client.client_id(),
            scopes,
        )
    }

    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    #[must_use]
    pub fn resource(&self) -> &str {
        &self.resource
    }

    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    #[must_use]
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }

    #[must_use]
    pub fn binding_id(&self) -> &str {
        &self.binding_id
    }

    fn keyring_account(&self) -> String {
        format!("scope-{}", self.binding_id)
    }
}

/// Versioned credential record held only in memory or the system credential store.
#[derive(Clone)]
pub struct McpOAuthCredentialRecord {
    scope: McpOAuthCredentialScope,
    access_token: Option<SecretString>,
    refresh_token: Option<SecretString>,
    expires_at_epoch_secs: Option<u64>,
    token_type: String,
    client_secret: Option<SecretString>,
    token_endpoint_auth_method: String,
    registration_access_token: Option<SecretString>,
    registration_client_uri: Option<String>,
    client_id_issued_at: Option<u64>,
    client_secret_expires_at: Option<u64>,
    token_endpoint: String,
    revocation_endpoint: Option<String>,
    rotation_id: String,
}

impl fmt::Debug for McpOAuthCredentialRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthCredentialRecord")
            .field("binding_id", &self.scope.binding_id)
            .field("has_access_token", &self.access_token.is_some())
            .field("has_refresh_token", &self.refresh_token.is_some())
            .field("has_client_secret", &self.client_secret.is_some())
            .field("expires_at_epoch_secs", &self.expires_at_epoch_secs)
            .finish_non_exhaustive()
    }
}

impl McpOAuthCredentialRecord {
    /// Creates a complete record from a successful authorization-code exchange.
    pub fn from_token_response(
        scope: McpOAuthCredentialScope,
        discovery: &McpOAuthDiscovery,
        client: &McpOAuthClientRegistration,
        token: &McpOAuthTokenResponse,
        now_epoch_secs: u64,
    ) -> Result<Self, McpOAuthCredentialError> {
        if scope.resource() != discovery.resource().as_str()
            || scope.issuer() != discovery.issuer()
            || scope.client_id() != client.client_id()
            || token
                .scopes()
                .iter()
                .any(|value| !scope.scopes().iter().any(|scope| scope == value))
        {
            return Err(McpOAuthCredentialError::InvalidRecord);
        }
        let expires_at_epoch_secs = match token.expires_in_secs() {
            Some(value) => Some(
                now_epoch_secs
                    .checked_add(value)
                    .ok_or(McpOAuthCredentialError::InvalidRecord)?,
            ),
            None => None,
        };
        let record = Self {
            scope,
            access_token: Some(token.access_token().clone()),
            refresh_token: token.refresh_token().cloned(),
            expires_at_epoch_secs,
            token_type: "Bearer".to_owned(),
            client_secret: client.client_secret().cloned(),
            token_endpoint_auth_method: client.token_endpoint_auth_method().to_owned(),
            registration_access_token: client.registration_access_token().cloned(),
            registration_client_uri: client.registration_client_uri().map(str::to_owned),
            client_id_issued_at: client.client_id_issued_at(),
            client_secret_expires_at: client.client_secret_expires_at(),
            token_endpoint: discovery.token_endpoint().to_owned(),
            revocation_endpoint: discovery.revocation_endpoint().map(str::to_owned),
            rotation_id: Uuid::new_v4().to_string(),
        };
        record.validate()?;
        Ok(record)
    }

    #[must_use]
    pub fn scope(&self) -> &McpOAuthCredentialScope {
        &self.scope
    }

    #[must_use]
    pub fn status(&self, now_epoch_secs: u64) -> McpOAuthCredentialStatus {
        if self.access_token.is_none() {
            return McpOAuthCredentialStatus::Expired;
        }
        match self.expires_at_epoch_secs {
            Some(expiry) if expiry <= now_epoch_secs => McpOAuthCredentialStatus::Expired,
            Some(expiry) if expiry <= now_epoch_secs.saturating_add(EXPIRY_SKEW_SECS) => {
                McpOAuthCredentialStatus::Expiring
            }
            _ => McpOAuthCredentialStatus::Present,
        }
    }

    #[must_use]
    pub fn has_refresh_token(&self) -> bool {
        self.refresh_token.is_some()
    }

    #[must_use]
    pub fn can_refresh(&self, now_epoch_secs: u64) -> bool {
        self.refresh_token.is_some()
            && (self.token_endpoint_auth_method == "none"
                || (self.client_secret.is_some()
                    && self
                        .client_secret_expires_at
                        .is_none_or(|expiry| expiry == 0 || expiry > now_epoch_secs)))
    }

    #[must_use]
    pub fn generation_id(&self) -> &str {
        &self.rotation_id
    }

    /// Creates one immutable bearer snapshot bound to the static-header fingerprint.
    pub fn snapshot(
        &self,
        static_header_fingerprint: &str,
        now_epoch_secs: u64,
    ) -> Result<McpOAuthCredentialSnapshot, McpOAuthCredentialError> {
        if !matches!(
            self.status(now_epoch_secs),
            McpOAuthCredentialStatus::Present
        ) {
            return Err(McpOAuthCredentialError::AuthenticationRequired);
        }
        if !valid_text(static_header_fingerprint, 1024)
            || static_header_fingerprint.chars().any(char::is_whitespace)
        {
            return Err(McpOAuthCredentialError::InvalidRecord);
        }
        let access_token = self
            .access_token
            .as_ref()
            .ok_or(McpOAuthCredentialError::AuthenticationRequired)?;
        let authorization = SecretString::new(format!("Bearer {}", access_token.expose_secret()));
        let key = CREDENTIAL_FINGERPRINT_KEY.get_or_init(process_random_key);
        let mut mac = Hmac::<Sha256>::new_from_slice(key)
            .map_err(|_| McpOAuthCredentialError::InvalidRecord)?;
        update_mac(&mut mac, "scope", self.scope.binding_id.as_bytes());
        update_mac(&mut mac, "rotation", self.rotation_id.as_bytes());
        update_mac(
            &mut mac,
            "static_headers",
            static_header_fingerprint.as_bytes(),
        );
        update_mac(
            &mut mac,
            "access_token",
            access_token.expose_secret().as_bytes(),
        );
        let live_fingerprint = format!("hmac-sha256:{:x}", mac.finalize().into_bytes());
        Ok(McpOAuthCredentialSnapshot {
            scope_binding_id: self.scope.binding_id.clone(),
            authorization,
            live_fingerprint,
        })
    }

    /// Rotates the complete record after a successful refresh response.
    pub fn rotated(
        &self,
        token: &McpOAuthTokenResponse,
        now_epoch_secs: u64,
    ) -> Result<Self, McpOAuthCredentialError> {
        if token
            .scopes()
            .iter()
            .any(|value| !self.scope.scopes.iter().any(|scope| scope == value))
        {
            return Err(McpOAuthCredentialError::InvalidRecord);
        }
        let mut next = self.clone();
        next.access_token = Some(token.access_token().clone());
        if let Some(refresh_token) = token.refresh_token() {
            next.refresh_token = Some(refresh_token.clone());
        }
        next.expires_at_epoch_secs = match token.expires_in_secs() {
            Some(value) => Some(
                now_epoch_secs
                    .checked_add(value)
                    .ok_or(McpOAuthCredentialError::InvalidRecord)?,
            ),
            None => None,
        };
        next.rotation_id = Uuid::new_v4().to_string();
        next.validate()?;
        Ok(next)
    }

    /// Returns a record that cannot be used or refreshed after an invalid refresh grant.
    #[must_use]
    pub fn without_usable_tokens(&self, now_epoch_secs: u64) -> Self {
        let mut next = self.clone();
        next.access_token = None;
        next.refresh_token = None;
        next.expires_at_epoch_secs = Some(now_epoch_secs);
        next.rotation_id = Uuid::new_v4().to_string();
        next
    }

    /// Invalidates only the access-token snapshot after a remote 401 without refreshing or retrying.
    #[must_use]
    pub fn without_access_token(&self, now_epoch_secs: u64) -> Self {
        let mut next = self.clone();
        next.access_token = None;
        next.expires_at_epoch_secs = Some(now_epoch_secs);
        next.rotation_id = Uuid::new_v4().to_string();
        next
    }

    fn validate(&self) -> Result<(), McpOAuthCredentialError> {
        if self.token_type != "Bearer"
            || !valid_https_binding(&self.token_endpoint)
            || self
                .revocation_endpoint
                .as_deref()
                .is_some_and(|value| !valid_https_binding(value))
            || !matches!(
                self.token_endpoint_auth_method.as_str(),
                "none" | "client_secret_post" | "client_secret_basic"
            )
            || (self.token_endpoint_auth_method != "none" && self.client_secret.is_none())
            || !valid_secret(self.access_token.as_ref())
            || !valid_secret(self.refresh_token.as_ref())
            || !valid_secret(self.client_secret.as_ref())
            || !valid_secret(self.registration_access_token.as_ref())
            || self
                .registration_client_uri
                .as_deref()
                .is_some_and(|value| !valid_https_binding(value))
            || Uuid::parse_str(&self.rotation_id).is_err()
        {
            return Err(McpOAuthCredentialError::InvalidRecord);
        }
        Ok(())
    }
}

/// Immutable per-request bearer carrier with a process-local HMAC fingerprint.
pub struct McpOAuthCredentialSnapshot {
    scope_binding_id: String,
    authorization: SecretString,
    live_fingerprint: String,
}

impl fmt::Debug for McpOAuthCredentialSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthCredentialSnapshot")
            .field("scope_binding_id", &self.scope_binding_id)
            .field("live_fingerprint", &self.live_fingerprint)
            .finish_non_exhaustive()
    }
}

impl McpOAuthCredentialSnapshot {
    #[must_use]
    pub fn scope_binding_id(&self) -> &str {
        &self.scope_binding_id
    }

    #[must_use]
    pub fn authorization(&self) -> &SecretString {
        &self.authorization
    }

    #[must_use]
    pub fn live_fingerprint(&self) -> &str {
        &self.live_fingerprint
    }
}

/// System-keyring-only persistence boundary for MCP OAuth credentials.
#[async_trait]
pub trait McpOAuthCredentialStore: Send + Sync {
    async fn load(
        &self,
        scope: &McpOAuthCredentialScope,
    ) -> Result<Option<McpOAuthCredentialRecord>, McpOAuthCredentialError>;

    async fn store(&self, record: &McpOAuthCredentialRecord)
    -> Result<(), McpOAuthCredentialError>;

    async fn delete(
        &self,
        scope: &McpOAuthCredentialScope,
    ) -> Result<bool, McpOAuthCredentialError>;
}

/// Keyring-only lookup index from public configuration intent to an exact credential scope.
#[async_trait]
pub trait McpOAuthCredentialLocatorStore: Send + Sync {
    async fn load_located(
        &self,
        lookup: &McpOAuthCredentialLookup,
    ) -> Result<Option<McpOAuthCredentialRecord>, McpOAuthCredentialError>;

    async fn store_locator(
        &self,
        lookup: &McpOAuthCredentialLookup,
        scope: &McpOAuthCredentialScope,
    ) -> Result<(), McpOAuthCredentialError>;

    async fn delete_locator(
        &self,
        lookup: &McpOAuthCredentialLookup,
    ) -> Result<bool, McpOAuthCredentialError>;
}

/// Native system credential-store implementation with no plaintext fallback.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemMcpOAuthCredentialStore;

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux"
))]
#[async_trait]
impl McpOAuthCredentialStore for SystemMcpOAuthCredentialStore {
    async fn load(
        &self,
        scope: &McpOAuthCredentialScope,
    ) -> Result<Option<McpOAuthCredentialRecord>, McpOAuthCredentialError> {
        let account = scope.keyring_account();
        let bytes = tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(CREDENTIAL_SERVICE, &account)
                .map_err(|_| McpOAuthCredentialError::StoreUnavailable)?;
            match entry.get_secret() {
                Ok(value) => Ok(Some(Zeroizing::new(value))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => Err(McpOAuthCredentialError::StoreUnavailable),
            }
        })
        .await
        .map_err(|_| McpOAuthCredentialError::StoreUnavailable)??;
        bytes.map(|bytes| decode_record(scope, &bytes)).transpose()
    }

    async fn store(
        &self,
        record: &McpOAuthCredentialRecord,
    ) -> Result<(), McpOAuthCredentialError> {
        let account = record.scope.keyring_account();
        let bytes = encode_record(record)?;
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(CREDENTIAL_SERVICE, &account)
                .map_err(|_| McpOAuthCredentialError::StoreUnavailable)?;
            entry
                .set_secret(&bytes)
                .map_err(|_| McpOAuthCredentialError::StoreRejected)
        })
        .await
        .map_err(|_| McpOAuthCredentialError::StoreUnavailable)??;
        Ok(())
    }

    async fn delete(
        &self,
        scope: &McpOAuthCredentialScope,
    ) -> Result<bool, McpOAuthCredentialError> {
        let account = scope.keyring_account();
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(CREDENTIAL_SERVICE, &account)
                .map_err(|_| McpOAuthCredentialError::StoreUnavailable)?;
            match entry.delete_credential() {
                Ok(()) => Ok(true),
                Err(keyring::Error::NoEntry) => Ok(false),
                Err(_) => Err(McpOAuthCredentialError::StoreRejected),
            }
        })
        .await
        .map_err(|_| McpOAuthCredentialError::StoreUnavailable)?
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux"
))]
#[async_trait]
impl McpOAuthCredentialLocatorStore for SystemMcpOAuthCredentialStore {
    async fn load_located(
        &self,
        lookup: &McpOAuthCredentialLookup,
    ) -> Result<Option<McpOAuthCredentialRecord>, McpOAuthCredentialError> {
        let account = lookup.keyring_account();
        let bytes = tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(CREDENTIAL_SERVICE, &account)
                .map_err(|_| McpOAuthCredentialError::StoreUnavailable)?;
            match entry.get_secret() {
                Ok(value) => Ok(Some(Zeroizing::new(value))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => Err(McpOAuthCredentialError::StoreUnavailable),
            }
        })
        .await
        .map_err(|_| McpOAuthCredentialError::StoreUnavailable)??;
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        let scope = decode_locator(lookup, &bytes)?;
        self.load(&scope).await
    }

    async fn store_locator(
        &self,
        lookup: &McpOAuthCredentialLookup,
        scope: &McpOAuthCredentialScope,
    ) -> Result<(), McpOAuthCredentialError> {
        if !lookup.accepts(scope) {
            return Err(McpOAuthCredentialError::InvalidScope);
        }
        let account = lookup.keyring_account();
        let bytes = encode_locator(scope)?;
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(CREDENTIAL_SERVICE, &account)
                .map_err(|_| McpOAuthCredentialError::StoreUnavailable)?;
            entry
                .set_secret(&bytes)
                .map_err(|_| McpOAuthCredentialError::StoreRejected)
        })
        .await
        .map_err(|_| McpOAuthCredentialError::StoreUnavailable)??;
        Ok(())
    }

    async fn delete_locator(
        &self,
        lookup: &McpOAuthCredentialLookup,
    ) -> Result<bool, McpOAuthCredentialError> {
        let account = lookup.keyring_account();
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(CREDENTIAL_SERVICE, &account)
                .map_err(|_| McpOAuthCredentialError::StoreUnavailable)?;
            match entry.delete_credential() {
                Ok(()) => Ok(true),
                Err(keyring::Error::NoEntry) => Ok(false),
                Err(_) => Err(McpOAuthCredentialError::StoreRejected),
            }
        })
        .await
        .map_err(|_| McpOAuthCredentialError::StoreUnavailable)?
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux"
)))]
#[async_trait]
impl McpOAuthCredentialStore for SystemMcpOAuthCredentialStore {
    async fn load(
        &self,
        _scope: &McpOAuthCredentialScope,
    ) -> Result<Option<McpOAuthCredentialRecord>, McpOAuthCredentialError> {
        Err(McpOAuthCredentialError::StoreUnavailable)
    }

    async fn store(
        &self,
        _record: &McpOAuthCredentialRecord,
    ) -> Result<(), McpOAuthCredentialError> {
        Err(McpOAuthCredentialError::StoreUnavailable)
    }

    async fn delete(
        &self,
        _scope: &McpOAuthCredentialScope,
    ) -> Result<bool, McpOAuthCredentialError> {
        Err(McpOAuthCredentialError::StoreUnavailable)
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux"
)))]
#[async_trait]
impl McpOAuthCredentialLocatorStore for SystemMcpOAuthCredentialStore {
    async fn load_located(
        &self,
        _lookup: &McpOAuthCredentialLookup,
    ) -> Result<Option<McpOAuthCredentialRecord>, McpOAuthCredentialError> {
        Err(McpOAuthCredentialError::StoreUnavailable)
    }

    async fn store_locator(
        &self,
        _lookup: &McpOAuthCredentialLookup,
        _scope: &McpOAuthCredentialScope,
    ) -> Result<(), McpOAuthCredentialError> {
        Err(McpOAuthCredentialError::StoreUnavailable)
    }

    async fn delete_locator(
        &self,
        _lookup: &McpOAuthCredentialLookup,
    ) -> Result<bool, McpOAuthCredentialError> {
        Err(McpOAuthCredentialError::StoreUnavailable)
    }
}

/// Outcome of an explicit remote revocation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpOAuthRevocationOutcome {
    NotAdvertised,
    Revoked,
}

/// Performs exactly one refresh-token request for a stored credential.
pub async fn refresh_oauth_credential(
    executor: &dyn McpOAuthHttpExecutor,
    record: &McpOAuthCredentialRecord,
) -> Result<McpOAuthTokenResponse, McpOAuthCredentialError> {
    let request = build_refresh_request(record)?;
    let response = executor
        .execute(request)
        .await
        .map_err(map_transport_error)?;
    if is_invalid_grant(&response) {
        return Err(McpOAuthCredentialError::InvalidRefresh);
    }
    parse_bearer_token_response(response, &record.scope.scopes, &record.scope.scopes).map_err(
        |error| match error {
            McpOAuthProtocolError::DestinationRejected => {
                McpOAuthCredentialError::DestinationRejected
            }
            McpOAuthProtocolError::BudgetExhausted => McpOAuthCredentialError::BudgetExhausted,
            McpOAuthProtocolError::Transport => McpOAuthCredentialError::Transport,
            _ => McpOAuthCredentialError::RefreshRejected,
        },
    )
}

fn build_refresh_request(
    record: &McpOAuthCredentialRecord,
) -> Result<McpOAuthHttpRequest, McpOAuthCredentialError> {
    let refresh_token = record
        .refresh_token
        .as_ref()
        .ok_or(McpOAuthCredentialError::AuthenticationRequired)?;
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("grant_type", "refresh_token");
    serializer.append_pair("refresh_token", refresh_token.expose_secret());
    serializer.append_pair("client_id", record.scope.client_id());
    serializer.append_pair("resource", record.scope.resource());
    if !record.scope.scopes.is_empty() {
        serializer.append_pair("scope", &record.scope.scopes.join(" "));
    }
    let mut headers = Vec::new();
    append_client_auth(record, &mut serializer, &mut headers)?;
    let body = SecretString::new(serializer.finish());
    let endpoint =
        Url::parse(&record.token_endpoint).map_err(|_| McpOAuthCredentialError::InvalidRecord)?;
    Ok(McpOAuthHttpRequest::post(
        &endpoint,
        McpOAuthHttpPurpose::TokenRefresh,
        "application/x-www-form-urlencoded",
        headers,
        body,
    ))
}

/// Performs at most one remote token revocation and never clears local state implicitly.
pub async fn revoke_oauth_credential(
    executor: &dyn McpOAuthHttpExecutor,
    record: &McpOAuthCredentialRecord,
) -> Result<McpOAuthRevocationOutcome, McpOAuthCredentialError> {
    let Some(request) = build_revocation_request(record)? else {
        return Ok(McpOAuthRevocationOutcome::NotAdvertised);
    };
    let response = executor
        .execute(request)
        .await
        .map_err(map_transport_error)?;
    if response.status != 200 {
        return Err(McpOAuthCredentialError::RevocationRejected);
    }
    Ok(McpOAuthRevocationOutcome::Revoked)
}

fn map_transport_error(error: McpOAuthTransportError) -> McpOAuthCredentialError {
    match error {
        McpOAuthTransportError::DestinationRejected => McpOAuthCredentialError::DestinationRejected,
        McpOAuthTransportError::BudgetExhausted => McpOAuthCredentialError::BudgetExhausted,
        McpOAuthTransportError::Transport => McpOAuthCredentialError::Transport,
    }
}

fn build_revocation_request(
    record: &McpOAuthCredentialRecord,
) -> Result<Option<McpOAuthHttpRequest>, McpOAuthCredentialError> {
    let Some(endpoint) = record.revocation_endpoint.as_deref() else {
        return Ok(None);
    };
    let token = record
        .refresh_token
        .as_ref()
        .or(record.access_token.as_ref())
        .ok_or(McpOAuthCredentialError::AuthenticationRequired)?;
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("token", token.expose_secret());
    serializer.append_pair(
        "token_type_hint",
        if record.refresh_token.is_some() {
            "refresh_token"
        } else {
            "access_token"
        },
    );
    serializer.append_pair("client_id", record.scope.client_id());
    let mut headers = Vec::new();
    append_client_auth(record, &mut serializer, &mut headers)?;
    let body = SecretString::new(serializer.finish());
    let endpoint = Url::parse(endpoint).map_err(|_| McpOAuthCredentialError::InvalidRecord)?;
    Ok(Some(McpOAuthHttpRequest::post(
        &endpoint,
        McpOAuthHttpPurpose::TokenRevocation,
        "application/x-www-form-urlencoded",
        headers,
        body,
    )))
}

fn append_client_auth(
    record: &McpOAuthCredentialRecord,
    serializer: &mut url::form_urlencoded::Serializer<'_, String>,
    headers: &mut Vec<(String, SecretString)>,
) -> Result<(), McpOAuthCredentialError> {
    match record.token_endpoint_auth_method.as_str() {
        "none" => {}
        "client_secret_post" => {
            serializer.append_pair(
                "client_secret",
                record
                    .client_secret
                    .as_ref()
                    .ok_or(McpOAuthCredentialError::InvalidRecord)?
                    .expose_secret(),
            );
        }
        "client_secret_basic" => {
            let secret = record
                .client_secret
                .as_ref()
                .ok_or(McpOAuthCredentialError::InvalidRecord)?;
            headers.push((
                "authorization".to_owned(),
                basic_client_authorization(record.scope.client_id(), secret),
            ));
        }
        _ => return Err(McpOAuthCredentialError::InvalidRecord),
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct OAuthErrorDocument {
    error: String,
}

fn is_invalid_grant(response: &McpOAuthHttpResponse) -> bool {
    response.status == 400
        && response.body.expose_secret().len() <= 16 * 1024
        && serde_json::from_str::<OAuthErrorDocument>(response.body.expose_secret())
            .is_ok_and(|document| document.error == "invalid_grant")
}

#[derive(Serialize)]
struct StoredCredentialRef<'a> {
    version: u8,
    server_name: &'a str,
    resource: &'a str,
    issuer: &'a str,
    client_id: &'a str,
    scopes: &'a [String],
    access_token: Option<&'a str>,
    refresh_token: Option<&'a str>,
    expires_at_epoch_secs: Option<u64>,
    token_type: &'a str,
    client_secret: Option<&'a str>,
    token_endpoint_auth_method: &'a str,
    registration_access_token: Option<&'a str>,
    registration_client_uri: Option<&'a str>,
    client_id_issued_at: Option<u64>,
    client_secret_expires_at: Option<u64>,
    token_endpoint: &'a str,
    revocation_endpoint: Option<&'a str>,
    rotation_id: &'a str,
}

#[derive(Deserialize)]
struct LoadedCredential {
    version: u8,
    server_name: String,
    resource: String,
    issuer: String,
    client_id: String,
    scopes: Vec<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_at_epoch_secs: Option<u64>,
    token_type: String,
    client_secret: Option<String>,
    token_endpoint_auth_method: String,
    registration_access_token: Option<String>,
    registration_client_uri: Option<String>,
    client_id_issued_at: Option<u64>,
    client_secret_expires_at: Option<u64>,
    token_endpoint: String,
    revocation_endpoint: Option<String>,
    rotation_id: String,
}

#[derive(Serialize)]
struct StoredCredentialLocatorRef<'a> {
    version: u8,
    server_name: &'a str,
    resource: &'a str,
    issuer: &'a str,
    client_id: &'a str,
    scopes: &'a [String],
}

#[derive(Deserialize)]
struct LoadedCredentialLocator {
    version: u8,
    server_name: String,
    resource: String,
    issuer: String,
    client_id: String,
    scopes: Vec<String>,
}

impl Drop for LoadedCredential {
    fn drop(&mut self) {
        if let Some(value) = self.access_token.as_mut() {
            value.zeroize();
        }
        if let Some(value) = self.refresh_token.as_mut() {
            value.zeroize();
        }
        if let Some(value) = self.client_secret.as_mut() {
            value.zeroize();
        }
        if let Some(value) = self.registration_access_token.as_mut() {
            value.zeroize();
        }
    }
}

fn encode_record(
    record: &McpOAuthCredentialRecord,
) -> Result<Zeroizing<Vec<u8>>, McpOAuthCredentialError> {
    record.validate()?;
    let wire = StoredCredentialRef {
        version: CREDENTIAL_RECORD_VERSION,
        server_name: record.scope.server_name(),
        resource: record.scope.resource(),
        issuer: record.scope.issuer(),
        client_id: record.scope.client_id(),
        scopes: record.scope.scopes(),
        access_token: record
            .access_token
            .as_ref()
            .map(SecretString::expose_secret),
        refresh_token: record
            .refresh_token
            .as_ref()
            .map(SecretString::expose_secret),
        expires_at_epoch_secs: record.expires_at_epoch_secs,
        token_type: &record.token_type,
        client_secret: record
            .client_secret
            .as_ref()
            .map(SecretString::expose_secret),
        token_endpoint_auth_method: &record.token_endpoint_auth_method,
        registration_access_token: record
            .registration_access_token
            .as_ref()
            .map(SecretString::expose_secret),
        registration_client_uri: record.registration_client_uri.as_deref(),
        client_id_issued_at: record.client_id_issued_at,
        client_secret_expires_at: record.client_secret_expires_at,
        token_endpoint: &record.token_endpoint,
        revocation_endpoint: record.revocation_endpoint.as_deref(),
        rotation_id: &record.rotation_id,
    };
    let encoded = Zeroizing::new(
        serde_json::to_vec(&wire).map_err(|_| McpOAuthCredentialError::InvalidRecord)?,
    );
    if encoded.len() > MAX_CREDENTIAL_RECORD_BYTES {
        return Err(McpOAuthCredentialError::InvalidRecord);
    }
    Ok(encoded)
}

fn encode_locator(
    scope: &McpOAuthCredentialScope,
) -> Result<Zeroizing<Vec<u8>>, McpOAuthCredentialError> {
    let encoded = Zeroizing::new(
        serde_json::to_vec(&StoredCredentialLocatorRef {
            version: CREDENTIAL_LOCATOR_VERSION,
            server_name: scope.server_name(),
            resource: scope.resource(),
            issuer: scope.issuer(),
            client_id: scope.client_id(),
            scopes: scope.scopes(),
        })
        .map_err(|_| McpOAuthCredentialError::InvalidRecord)?,
    );
    if encoded.is_empty() || encoded.len() > MAX_CREDENTIAL_RECORD_BYTES {
        return Err(McpOAuthCredentialError::InvalidRecord);
    }
    Ok(encoded)
}

fn decode_locator(
    lookup: &McpOAuthCredentialLookup,
    bytes: &[u8],
) -> Result<McpOAuthCredentialScope, McpOAuthCredentialError> {
    if bytes.is_empty() || bytes.len() > MAX_CREDENTIAL_RECORD_BYTES {
        return Err(McpOAuthCredentialError::InvalidRecord);
    }
    let loaded: LoadedCredentialLocator =
        serde_json::from_slice(bytes).map_err(|_| McpOAuthCredentialError::InvalidRecord)?;
    if loaded.version != CREDENTIAL_LOCATOR_VERSION {
        return Err(McpOAuthCredentialError::InvalidRecord);
    }
    let scope = McpOAuthCredentialScope::new(
        loaded.server_name,
        loaded.resource,
        loaded.issuer,
        loaded.client_id,
        loaded.scopes,
    )?;
    if !lookup.accepts(&scope) {
        return Err(McpOAuthCredentialError::InvalidRecord);
    }
    Ok(scope)
}

fn decode_record(
    expected_scope: &McpOAuthCredentialScope,
    bytes: &[u8],
) -> Result<McpOAuthCredentialRecord, McpOAuthCredentialError> {
    if bytes.is_empty() || bytes.len() > MAX_CREDENTIAL_RECORD_BYTES {
        return Err(McpOAuthCredentialError::InvalidRecord);
    }
    let loaded: LoadedCredential =
        serde_json::from_slice(bytes).map_err(|_| McpOAuthCredentialError::InvalidRecord)?;
    if loaded.version != CREDENTIAL_RECORD_VERSION {
        return Err(McpOAuthCredentialError::InvalidRecord);
    }
    let scope = McpOAuthCredentialScope::new(
        loaded.server_name.clone(),
        loaded.resource.clone(),
        loaded.issuer.clone(),
        loaded.client_id.clone(),
        loaded.scopes.clone(),
    )?;
    if &scope != expected_scope {
        return Err(McpOAuthCredentialError::InvalidRecord);
    }
    let record = McpOAuthCredentialRecord {
        scope,
        access_token: loaded
            .access_token
            .as_ref()
            .map(|value| SecretString::new(value.clone())),
        refresh_token: loaded
            .refresh_token
            .as_ref()
            .map(|value| SecretString::new(value.clone())),
        expires_at_epoch_secs: loaded.expires_at_epoch_secs,
        token_type: loaded.token_type.clone(),
        client_secret: loaded
            .client_secret
            .as_ref()
            .map(|value| SecretString::new(value.clone())),
        token_endpoint_auth_method: loaded.token_endpoint_auth_method.clone(),
        registration_access_token: loaded
            .registration_access_token
            .as_ref()
            .map(|value| SecretString::new(value.clone())),
        registration_client_uri: loaded.registration_client_uri.clone(),
        client_id_issued_at: loaded.client_id_issued_at,
        client_secret_expires_at: loaded.client_secret_expires_at,
        token_endpoint: loaded.token_endpoint.clone(),
        revocation_endpoint: loaded.revocation_endpoint.clone(),
        rotation_id: loaded.rotation_id.clone(),
    };
    record.validate()?;
    Ok(record)
}

fn normalize_scopes(mut scopes: Vec<String>) -> Result<Vec<String>, McpOAuthCredentialError> {
    if scopes.len() > MAX_SCOPES {
        return Err(McpOAuthCredentialError::InvalidScope);
    }
    let mut total = 0usize;
    for scope in &scopes {
        total = total.saturating_add(scope.len());
        if scope.is_empty()
            || scope.len() > MAX_SCOPE_BYTES
            || total > MAX_SCOPE_TOTAL_BYTES
            || !scope.bytes().all(|byte| {
                byte == 0x21 || (0x23..=0x5b).contains(&byte) || (0x5d..=0x7e).contains(&byte)
            })
        {
            return Err(McpOAuthCredentialError::InvalidScope);
        }
    }
    scopes.sort_unstable();
    if scopes.windows(2).any(|values| values[0] == values[1]) {
        return Err(McpOAuthCredentialError::InvalidScope);
    }
    Ok(scopes)
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn valid_https_binding(value: &str) -> bool {
    valid_text(value, MAX_BINDING_TEXT_BYTES)
        && Url::parse(value).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
        })
}

fn valid_issuer_binding(value: &str) -> bool {
    valid_https_binding(value) && Url::parse(value).is_ok_and(|url| url.query().is_none())
}

fn valid_secret(value: Option<&SecretString>) -> bool {
    value.is_none_or(|value| {
        let value = value.expose_secret();
        !value.is_empty()
            && value.len() <= MAX_SECRET_BYTES
            && !value
                .bytes()
                .any(|byte| byte == 0 || byte == b'\r' || byte == b'\n' || byte < 0x20)
    })
}

fn scope_binding_id(
    server_name: &str,
    resource: &str,
    issuer: &str,
    client_id: &str,
    scopes: &[String],
) -> String {
    let mut digest = Sha256::new();
    update_digest(&mut digest, "server", server_name.as_bytes());
    update_digest(&mut digest, "resource", resource.as_bytes());
    update_digest(&mut digest, "issuer", issuer.as_bytes());
    update_digest(&mut digest, "client", client_id.as_bytes());
    for scope in scopes {
        update_digest(&mut digest, "scope", scope.as_bytes());
    }
    format!("sha256:{:x}", digest.finalize())
}

fn update_digest(digest: &mut Sha256, label: &str, value: &[u8]) {
    digest.update((label.len() as u64).to_be_bytes());
    digest.update(label.as_bytes());
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn update_mac(mac: &mut Hmac<Sha256>, label: &str, value: &[u8]) {
    mac.update(&(label.len() as u64).to_be_bytes());
    mac.update(label.as_bytes());
    mac.update(&(value.len() as u64).to_be_bytes());
    mac.update(value);
}

fn process_random_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    key[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    key
}

#[cfg(test)]
#[path = "../tests/oauth_credential_tests.rs"]
mod tests;
