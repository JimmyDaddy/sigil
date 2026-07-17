use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    net::Ipv4Addr,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sigil_kernel::SecretString;
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use url::Url;
use uuid::Uuid;

use super::auth::parse_auth_parameters;

const MAX_CHALLENGE_BYTES: usize = 4 * 1024;
const MAX_METADATA_BODY_BYTES: usize = 64 * 1024;
const MAX_TOKEN_BODY_BYTES: usize = 128 * 1024;
const MAX_CALLBACK_REQUEST_BYTES: usize = 16 * 1024;
const MAX_CALLBACK_URL_BYTES: usize = 8 * 1024;
const MAX_CLIENT_ID_BYTES: usize = 1024;
const MAX_TOKEN_BYTES: usize = 64 * 1024;
const MAX_SCOPE_BYTES: usize = 256;
const MAX_SCOPE_COUNT: usize = 32;
const MAX_SCOPE_TOTAL_BYTES: usize = 4 * 1024;
const MAX_ENDPOINT_BYTES: usize = 8 * 1024;
const MAX_AUTHORIZATION_URL_BYTES: usize = 16 * 1024;
const MAX_AUTHORIZATION_SERVERS: usize = 8;
const MAX_METADATA_LIST_ITEMS: usize = 64;
const MAX_TOKEN_LIFETIME_SECS: u64 = 366 * 24 * 60 * 60;
const OAUTH_FLOW_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
/// Stable, redaction-safe failures produced by the MCP OAuth protocol state machine.
pub enum McpOAuthProtocolError {
    #[error("remote MCP OAuth resource is invalid")]
    InvalidResource,
    #[error("remote MCP OAuth challenge is invalid or exceeds limits")]
    InvalidChallenge,
    #[error("remote MCP OAuth metadata is unavailable")]
    MetadataUnavailable,
    #[error("remote MCP OAuth metadata is invalid or exceeds limits")]
    InvalidMetadata,
    #[error("remote MCP OAuth metadata exposes multiple authorization servers")]
    AmbiguousAuthorizationServer,
    #[error("remote MCP authorization server does not support the required secure profile")]
    UnsupportedAuthorizationServer,
    #[error("remote MCP OAuth client configuration is invalid")]
    InvalidClient,
    #[error("remote MCP dynamic client registration was rejected")]
    ClientRegistrationRejected,
    #[error("remote MCP OAuth authorization response is invalid")]
    InvalidAuthorizationResponse,
    #[error("remote MCP OAuth authorization was rejected")]
    AuthorizationRejected,
    #[error("remote MCP OAuth flow expired")]
    FlowExpired,
    #[error("remote MCP OAuth flow was already consumed")]
    FlowConsumed,
    #[error("remote MCP OAuth callback listener failed")]
    CallbackFailed,
    #[error("remote MCP OAuth token exchange was rejected")]
    TokenRejected,
    #[error("remote MCP OAuth transport failed")]
    Transport,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
/// Failures reported by the runtime-owned OAuth HTTP transport boundary.
pub enum McpOAuthTransportError {
    #[error("OAuth destination authorization was rejected")]
    DestinationRejected,
    #[error("OAuth network budget is exhausted")]
    BudgetExhausted,
    #[error("OAuth transport failed")]
    Transport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// HTTP methods emitted by the bounded OAuth protocol client.
pub enum McpOAuthHttpMethod {
    Get,
    Post,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Security purpose attached to each independently authorized OAuth destination.
pub enum McpOAuthHttpPurpose {
    ProtectedResourceMetadata,
    AuthorizationServerMetadata,
    DynamicClientRegistration,
    TokenExchange,
}

/// One redaction-safe OAuth HTTP request for execution by the runtime transport.
pub struct McpOAuthHttpRequest {
    method: McpOAuthHttpMethod,
    purpose: McpOAuthHttpPurpose,
    destination: SecretString,
    content_type: Option<&'static str>,
    headers: Vec<(String, SecretString)>,
    body: Option<SecretString>,
}

impl fmt::Debug for McpOAuthHttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthHttpRequest")
            .field("method", &self.method)
            .field("purpose", &self.purpose)
            .field(
                "header_names",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field(
                "body_bytes",
                &self.body.as_ref().map(|body| body.expose_secret().len()),
            )
            .finish_non_exhaustive()
    }
}

impl McpOAuthHttpRequest {
    fn get(destination: &Url, purpose: McpOAuthHttpPurpose) -> Self {
        Self {
            method: McpOAuthHttpMethod::Get,
            purpose,
            destination: SecretString::new(destination.as_str()),
            content_type: None,
            headers: Vec::new(),
            body: None,
        }
    }

    fn post(
        destination: &Url,
        purpose: McpOAuthHttpPurpose,
        content_type: &'static str,
        headers: Vec<(String, SecretString)>,
        body: SecretString,
    ) -> Self {
        Self {
            method: McpOAuthHttpMethod::Post,
            purpose,
            destination: SecretString::new(destination.as_str()),
            content_type: Some(content_type),
            headers,
            body: Some(body),
        }
    }

    #[must_use]
    pub fn method(&self) -> McpOAuthHttpMethod {
        self.method
    }

    #[must_use]
    pub fn purpose(&self) -> McpOAuthHttpPurpose {
        self.purpose
    }

    #[must_use]
    pub fn destination(&self) -> &str {
        self.destination.expose_secret()
    }

    #[must_use]
    pub fn content_type(&self) -> Option<&'static str> {
        self.content_type
    }

    #[must_use]
    pub fn headers(&self) -> &[(String, SecretString)] {
        &self.headers
    }

    #[must_use]
    pub fn body(&self) -> Option<&str> {
        self.body.as_ref().map(SecretString::expose_secret)
    }
}

/// A bounded OAuth HTTP response returned by the runtime transport.
pub struct McpOAuthHttpResponse {
    pub status: u16,
    pub content_type: Option<String>,
    body: SecretString,
}

impl McpOAuthHttpResponse {
    #[must_use]
    pub fn new(status: u16, content_type: Option<String>, body: SecretString) -> Self {
        Self {
            status,
            content_type,
            body,
        }
    }
}

impl fmt::Debug for McpOAuthHttpResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthHttpResponse")
            .field("status", &self.status)
            .field("content_type", &self.content_type)
            .field("body_bytes", &self.body.expose_secret().len())
            .finish()
    }
}

#[async_trait]
/// Executes a single OAuth request after runtime egress authorization.
///
/// Implementations own the physical HTTP client and must not follow redirects or retry requests.
pub trait McpOAuthHttpExecutor: Send + Sync {
    /// Executes exactly one physical request after the runtime has independently authorized the
    /// request's destination. Implementations must disable redirects, retries, cookies and
    /// referrer propagation and must hard-cap the response before constructing it.
    async fn execute(
        &self,
        request: McpOAuthHttpRequest,
    ) -> Result<McpOAuthHttpResponse, McpOAuthTransportError>;
}

#[derive(Clone, PartialEq, Eq)]
/// Canonical HTTPS protected-resource identifier used for RFC 8707 binding.
pub struct McpOAuthResource {
    url: Url,
}

impl McpOAuthResource {
    pub fn parse(value: &str) -> Result<Self, McpOAuthProtocolError> {
        let url =
            validate_https_url(value, true).map_err(|_| McpOAuthProtocolError::InvalidResource)?;
        Ok(Self { url })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }
}

impl fmt::Debug for McpOAuthResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthResource")
            .field("origin", &safe_origin(&self.url))
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
/// Parsed Bearer challenge that owns no executable network behavior.
pub struct McpOAuthChallenge {
    resource: SecretString,
    resource_metadata: Option<Url>,
    scopes: Vec<String>,
}

impl fmt::Debug for McpOAuthChallenge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthChallenge")
            .field("has_resource_metadata", &self.resource_metadata.is_some())
            .field("scope_count", &self.scopes.len())
            .finish_non_exhaustive()
    }
}

impl McpOAuthChallenge {
    pub fn parse(
        header: &str,
        resource: SecretString,
    ) -> Result<Option<Self>, McpOAuthProtocolError> {
        if header.is_empty()
            || header.len() > MAX_CHALLENGE_BYTES
            || header.chars().any(char::is_control)
        {
            return Err(McpOAuthProtocolError::InvalidChallenge);
        }
        let (scheme, parameters) = header
            .split_once(char::is_whitespace)
            .unwrap_or((header, ""));
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Ok(None);
        }
        let parsed = parse_auth_parameters(parameters)
            .map_err(|_| McpOAuthProtocolError::InvalidChallenge)?;
        let resource_metadata = parsed
            .get("resource_metadata")
            .map(|value| validate_https_url(value, true))
            .transpose()
            .map_err(|_| McpOAuthProtocolError::InvalidChallenge)?;
        let scopes = parsed
            .get("scope")
            .map(|scope| validate_scopes(scope.split(' ').filter(|value| !value.is_empty())))
            .transpose()
            .map_err(|_| McpOAuthProtocolError::InvalidChallenge)?
            .unwrap_or_default();
        Ok(Some(Self {
            resource,
            resource_metadata,
            scopes,
        }))
    }

    #[must_use]
    pub fn resource(&self) -> &str {
        self.resource.expose_secret()
    }

    #[must_use]
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// User configuration relevant to OAuth client selection and requested scopes.
pub struct McpOAuthClientIntent {
    client_id: Option<String>,
    scopes: Vec<String>,
}

impl McpOAuthClientIntent {
    pub fn new(
        client_id: Option<String>,
        scopes: Vec<String>,
    ) -> Result<Self, McpOAuthProtocolError> {
        if let Some(client_id) = client_id.as_deref() {
            validate_client_id(client_id)?;
        }
        let scopes = validate_scopes(scopes.iter().map(String::as_str))?;
        Ok(Self { client_id, scopes })
    }

    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        self.client_id.as_deref()
    }

    #[must_use]
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }
}

#[derive(Clone)]
/// Validated protected-resource and authorization-server discovery result.
pub struct McpOAuthDiscovery {
    resource: McpOAuthResource,
    resource_metadata_endpoint: Url,
    issuer: Url,
    authorization_endpoint: Url,
    token_endpoint: Url,
    registration_endpoint: Option<Url>,
    revocation_endpoint: Option<Url>,
    token_auth_methods: Vec<String>,
    challenge_scopes: Vec<String>,
    resource_scopes: Vec<String>,
}

impl fmt::Debug for McpOAuthDiscovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthDiscovery")
            .field("resource_origin", &safe_origin(&self.resource.url))
            .field("issuer_origin", &safe_origin(&self.issuer))
            .field("has_registration", &self.registration_endpoint.is_some())
            .field("has_revocation", &self.revocation_endpoint.is_some())
            .finish_non_exhaustive()
    }
}

impl McpOAuthDiscovery {
    #[must_use]
    pub fn resource(&self) -> &McpOAuthResource {
        &self.resource
    }

    #[must_use]
    pub fn resource_metadata_endpoint(&self) -> &str {
        self.resource_metadata_endpoint.as_str()
    }

    #[must_use]
    pub fn issuer(&self) -> &str {
        self.issuer.as_str()
    }

    #[must_use]
    pub fn authorization_endpoint(&self) -> &str {
        self.authorization_endpoint.as_str()
    }

    #[must_use]
    pub fn token_endpoint(&self) -> &str {
        self.token_endpoint.as_str()
    }

    #[must_use]
    pub fn revocation_endpoint(&self) -> Option<&str> {
        self.revocation_endpoint.as_ref().map(Url::as_str)
    }

    #[must_use]
    pub fn resource_scopes(&self) -> &[String] {
        &self.resource_scopes
    }

    pub fn requested_scopes(
        &self,
        intent: &McpOAuthClientIntent,
    ) -> Result<Vec<String>, McpOAuthProtocolError> {
        let selected = if !self.challenge_scopes.is_empty() {
            self.challenge_scopes.clone()
        } else {
            intent.scopes.clone()
        };
        validate_scopes(selected.iter().map(String::as_str))
    }
}

#[derive(Debug, Deserialize)]
struct ProtectedResourceDocument {
    resource: String,
    #[serde(default)]
    authorization_servers: Vec<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AuthorizationServerDocument {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
    #[serde(default)]
    revocation_endpoint: Option<String>,
    response_types_supported: Vec<String>,
    #[serde(default)]
    grant_types_supported: Option<Vec<String>>,
    #[serde(default)]
    code_challenge_methods_supported: Vec<String>,
    #[serde(default)]
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
    #[serde(default)]
    protected_resources: Option<Vec<String>>,
}

/// Discovers and validates RFC 9728 and RFC 8414/OIDC metadata.
///
/// Every candidate destination is emitted separately through `executor`; only an exact resource,
/// issuer, HTTPS endpoint and PKCE S256-capable authorization server is accepted.
pub async fn discover_oauth_authorization_server(
    executor: &dyn McpOAuthHttpExecutor,
    challenge: &McpOAuthChallenge,
) -> Result<McpOAuthDiscovery, McpOAuthProtocolError> {
    let resource = McpOAuthResource::parse(challenge.resource.expose_secret())?;
    let metadata_candidates = if let Some(metadata) = challenge.resource_metadata.as_ref() {
        vec![metadata.clone()]
    } else {
        protected_resource_metadata_candidates(&resource.url)?
    };
    let (resource_metadata_endpoint, document): (_, ProtectedResourceDocument) = fetch_first_json(
        executor,
        metadata_candidates,
        McpOAuthHttpPurpose::ProtectedResourceMetadata,
        MAX_METADATA_BODY_BYTES,
    )
    .await?;
    if document.resource != resource.as_str()
        || document.authorization_servers.is_empty()
        || document.authorization_servers.len() > MAX_AUTHORIZATION_SERVERS
    {
        return Err(McpOAuthProtocolError::InvalidMetadata);
    }
    if document.authorization_servers.len() != 1 {
        return Err(McpOAuthProtocolError::AmbiguousAuthorizationServer);
    }
    let issuer = validate_issuer(&document.authorization_servers[0])?;
    let (metadata_endpoint, authorization): (_, AuthorizationServerDocument) = fetch_first_json(
        executor,
        authorization_server_metadata_candidates(&issuer)?,
        McpOAuthHttpPurpose::AuthorizationServerMetadata,
        MAX_METADATA_BODY_BYTES,
    )
    .await?;
    let _ = metadata_endpoint;
    if authorization.issuer != issuer.as_str()
        || !valid_metadata_list(&authorization.response_types_supported)
        || !authorization
            .response_types_supported
            .iter()
            .any(|value| value == "code")
        || authorization
            .grant_types_supported
            .as_ref()
            .is_some_and(|values| !valid_metadata_list(values))
        || authorization
            .grant_types_supported
            .as_ref()
            .is_some_and(|values| !values.iter().any(|value| value == "authorization_code"))
        || !valid_metadata_list(&authorization.code_challenge_methods_supported)
        || !authorization
            .code_challenge_methods_supported
            .iter()
            .any(|value| value == "S256")
    {
        return Err(McpOAuthProtocolError::UnsupportedAuthorizationServer);
    }
    if let Some(protected_resources) = authorization.protected_resources.as_ref()
        && (!valid_metadata_list(protected_resources)
            || !protected_resources
                .iter()
                .any(|value| value == resource.as_str()))
    {
        return Err(McpOAuthProtocolError::InvalidMetadata);
    }
    let authorization_endpoint = validate_https_url(&authorization.authorization_endpoint, true)
        .map_err(|_| McpOAuthProtocolError::InvalidMetadata)?;
    let token_endpoint = validate_https_url(&authorization.token_endpoint, true)
        .map_err(|_| McpOAuthProtocolError::InvalidMetadata)?;
    let registration_endpoint = authorization
        .registration_endpoint
        .as_deref()
        .map(|value| validate_https_url(value, true))
        .transpose()
        .map_err(|_| McpOAuthProtocolError::InvalidMetadata)?;
    let revocation_endpoint = authorization
        .revocation_endpoint
        .as_deref()
        .map(|value| validate_https_url(value, true))
        .transpose()
        .map_err(|_| McpOAuthProtocolError::InvalidMetadata)?;
    let token_auth_methods = authorization
        .token_endpoint_auth_methods_supported
        .unwrap_or_else(|| vec!["client_secret_basic".to_owned()]);
    if !valid_metadata_list(&token_auth_methods)
        || token_auth_methods
            .iter()
            .any(|value| value.chars().any(char::is_whitespace))
    {
        return Err(McpOAuthProtocolError::InvalidMetadata);
    }
    let resource_scopes = validate_scopes(document.scopes_supported.iter().map(String::as_str))
        .map_err(|_| McpOAuthProtocolError::InvalidMetadata)?;
    Ok(McpOAuthDiscovery {
        resource,
        resource_metadata_endpoint,
        issuer,
        authorization_endpoint,
        token_endpoint,
        registration_endpoint,
        revocation_endpoint,
        token_auth_methods,
        challenge_scopes: challenge.scopes.clone(),
        resource_scopes,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientAuthMethod {
    None,
    SecretPost,
    SecretBasic,
}

impl ClientAuthMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SecretPost => "client_secret_post",
            Self::SecretBasic => "client_secret_basic",
        }
    }
}

#[derive(Clone)]
/// Selected static client or validated dynamic client registration result.
pub struct McpOAuthClientRegistration {
    client_id: String,
    client_secret: Option<SecretString>,
    auth_method: ClientAuthMethod,
    registration_access_token: Option<SecretString>,
    registration_client_uri: Option<Url>,
    client_id_issued_at: Option<u64>,
    client_secret_expires_at: Option<u64>,
}

impl fmt::Debug for McpOAuthClientRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthClientRegistration")
            .field("client_id_bytes", &self.client_id.len())
            .field("auth_method", &self.auth_method.as_str())
            .field("has_client_secret", &self.client_secret.is_some())
            .field(
                "has_registration_access_token",
                &self.registration_access_token.is_some(),
            )
            .field(
                "has_registration_client_uri",
                &self.registration_client_uri.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl McpOAuthClientRegistration {
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    #[must_use]
    pub fn has_client_secret(&self) -> bool {
        self.client_secret.is_some()
    }

    #[must_use]
    pub fn client_secret(&self) -> Option<&SecretString> {
        self.client_secret.as_ref()
    }

    #[must_use]
    pub fn token_endpoint_auth_method(&self) -> &str {
        self.auth_method.as_str()
    }

    #[must_use]
    pub fn registration_access_token(&self) -> Option<&SecretString> {
        self.registration_access_token.as_ref()
    }

    #[must_use]
    pub fn registration_client_uri(&self) -> Option<&str> {
        self.registration_client_uri.as_ref().map(Url::as_str)
    }

    #[must_use]
    pub fn client_id_issued_at(&self) -> Option<u64> {
        self.client_id_issued_at
    }

    #[must_use]
    pub fn client_secret_expires_at(&self) -> Option<u64> {
        self.client_secret_expires_at
    }
}

#[derive(Debug, Deserialize)]
struct ClientRegistrationDocument {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
    token_endpoint_auth_method: String,
    #[serde(default)]
    registration_access_token: Option<String>,
    #[serde(default)]
    registration_client_uri: Option<String>,
    #[serde(default)]
    client_id_issued_at: Option<u64>,
    #[serde(default)]
    client_secret_expires_at: Option<u64>,
}

/// Selects a configured public client or performs metadata-advertised dynamic registration.
pub async fn prepare_oauth_client(
    executor: &dyn McpOAuthHttpExecutor,
    discovery: &McpOAuthDiscovery,
    intent: &McpOAuthClientIntent,
    redirect_uri: &str,
) -> Result<McpOAuthClientRegistration, McpOAuthProtocolError> {
    validate_loopback_redirect(redirect_uri)?;
    if let Some(client_id) = intent.client_id.as_ref() {
        if !discovery
            .token_auth_methods
            .iter()
            .any(|value| value == "none")
        {
            return Err(McpOAuthProtocolError::UnsupportedAuthorizationServer);
        }
        return Ok(McpOAuthClientRegistration {
            client_id: client_id.clone(),
            client_secret: None,
            auth_method: ClientAuthMethod::None,
            registration_access_token: None,
            registration_client_uri: None,
            client_id_issued_at: None,
            client_secret_expires_at: None,
        });
    }
    let endpoint = discovery
        .registration_endpoint
        .as_ref()
        .ok_or(McpOAuthProtocolError::InvalidClient)?;
    let auth_method = if discovery
        .token_auth_methods
        .iter()
        .any(|value| value == "none")
    {
        ClientAuthMethod::None
    } else if discovery
        .token_auth_methods
        .iter()
        .any(|value| value == "client_secret_post")
    {
        ClientAuthMethod::SecretPost
    } else if discovery
        .token_auth_methods
        .iter()
        .any(|value| value == "client_secret_basic")
    {
        ClientAuthMethod::SecretBasic
    } else {
        return Err(McpOAuthProtocolError::UnsupportedAuthorizationServer);
    };
    let body = serde_json::to_string(&serde_json::json!({
        "client_name": "Sigil",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code"],
        "response_types": ["code"],
        "token_endpoint_auth_method": auth_method.as_str(),
    }))
    .map_err(|_| McpOAuthProtocolError::InvalidClient)?;
    let response = executor
        .execute(McpOAuthHttpRequest::post(
            endpoint,
            McpOAuthHttpPurpose::DynamicClientRegistration,
            "application/json",
            Vec::new(),
            SecretString::new(body),
        ))
        .await
        .map_err(|_| McpOAuthProtocolError::Transport)?;
    if response.status != 201
        || !is_json_content_type(response.content_type.as_deref())
        || response.body.expose_secret().len() > MAX_METADATA_BODY_BYTES
    {
        return Err(McpOAuthProtocolError::ClientRegistrationRejected);
    }
    let registration: ClientRegistrationDocument =
        serde_json::from_str(response.body.expose_secret())
            .map_err(|_| McpOAuthProtocolError::ClientRegistrationRejected)?;
    validate_client_id(&registration.client_id)
        .map_err(|_| McpOAuthProtocolError::ClientRegistrationRejected)?;
    if registration.token_endpoint_auth_method != auth_method.as_str() {
        return Err(McpOAuthProtocolError::ClientRegistrationRejected);
    }
    let client_secret = registration
        .client_secret
        .map(validate_token)
        .transpose()
        .map_err(|_| McpOAuthProtocolError::ClientRegistrationRejected)?
        .map(SecretString::new);
    if auth_method != ClientAuthMethod::None && client_secret.is_none() {
        return Err(McpOAuthProtocolError::ClientRegistrationRejected);
    }
    let registration_access_token = registration
        .registration_access_token
        .map(validate_token)
        .transpose()
        .map_err(|_| McpOAuthProtocolError::ClientRegistrationRejected)?
        .map(SecretString::new);
    let registration_client_uri = registration
        .registration_client_uri
        .as_deref()
        .map(|value| validate_https_url(value, true))
        .transpose()
        .map_err(|_| McpOAuthProtocolError::ClientRegistrationRejected)?;
    Ok(McpOAuthClientRegistration {
        client_id: registration.client_id,
        client_secret,
        auth_method,
        registration_access_token,
        registration_client_uri,
        client_id_issued_at: registration.client_id_issued_at,
        client_secret_expires_at: registration.client_secret_expires_at,
    })
}

/// Single-use PKCE authorization flow bound to resource, client, redirect URI and scopes.
pub struct McpOAuthPendingAuthorization {
    flow_id: String,
    discovery: McpOAuthDiscovery,
    client: McpOAuthClientRegistration,
    scopes: Vec<String>,
    redirect_uri: Url,
    state: SecretString,
    verifier: SecretString,
    authorization_url: SecretString,
    expires_at: Instant,
    consumed: bool,
}

impl fmt::Debug for McpOAuthPendingAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthPendingAuthorization")
            .field("flow_id", &self.flow_id)
            .field("scope_count", &self.scopes.len())
            .field("consumed", &self.consumed)
            .finish_non_exhaustive()
    }
}

impl McpOAuthPendingAuthorization {
    pub fn new(
        discovery: McpOAuthDiscovery,
        client: McpOAuthClientRegistration,
        scopes: Vec<String>,
        redirect_uri: &str,
    ) -> Result<Self, McpOAuthProtocolError> {
        Self::new_with_ttl(discovery, client, scopes, redirect_uri, OAUTH_FLOW_TTL)
    }

    fn new_with_ttl(
        discovery: McpOAuthDiscovery,
        client: McpOAuthClientRegistration,
        scopes: Vec<String>,
        redirect_uri: &str,
        ttl: Duration,
    ) -> Result<Self, McpOAuthProtocolError> {
        let redirect_uri = validate_loopback_redirect(redirect_uri)?;
        let scopes = validate_scopes(scopes.iter().map(String::as_str))?;
        let verifier = random_base64url_32();
        let state = random_base64url_32();
        let challenge =
            general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut authorization_url = discovery.authorization_endpoint.clone();
        let owned_parameters = [
            "response_type",
            "client_id",
            "redirect_uri",
            "code_challenge",
            "code_challenge_method",
            "state",
            "resource",
            "scope",
        ];
        if authorization_url
            .query_pairs()
            .any(|(name, _)| owned_parameters.contains(&name.as_ref()))
        {
            return Err(McpOAuthProtocolError::InvalidMetadata);
        }
        {
            let mut query = authorization_url.query_pairs_mut();
            query.append_pair("response_type", "code");
            query.append_pair("client_id", client.client_id());
            query.append_pair("redirect_uri", redirect_uri.as_str());
            query.append_pair("code_challenge", &challenge);
            query.append_pair("code_challenge_method", "S256");
            query.append_pair("state", &state);
            query.append_pair("resource", discovery.resource.as_str());
            if !scopes.is_empty() {
                query.append_pair("scope", &scopes.join(" "));
            }
        }
        if authorization_url.as_str().len() > MAX_AUTHORIZATION_URL_BYTES {
            return Err(McpOAuthProtocolError::InvalidClient);
        }
        Ok(Self {
            flow_id: Uuid::new_v4().to_string(),
            discovery,
            client,
            scopes,
            redirect_uri,
            state: SecretString::new(state),
            verifier: SecretString::new(verifier),
            authorization_url: SecretString::new(authorization_url.to_string()),
            expires_at: Instant::now() + ttl,
            consumed: false,
        })
    }

    #[must_use]
    pub fn flow_id(&self) -> &str {
        &self.flow_id
    }

    #[must_use]
    pub fn authorization_url(&self) -> SecretString {
        self.authorization_url.clone()
    }

    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        self.redirect_uri.as_str()
    }

    pub fn complete_callback(
        &mut self,
        callback_url: SecretString,
    ) -> Result<McpOAuthAuthorizationCode, McpOAuthProtocolError> {
        self.consume()?;
        let raw = callback_url.expose_secret();
        if raw.is_empty() || raw.len() > MAX_CALLBACK_URL_BYTES || raw.chars().any(char::is_control)
        {
            return Err(McpOAuthProtocolError::InvalidAuthorizationResponse);
        }
        let callback =
            Url::parse(raw).map_err(|_| McpOAuthProtocolError::InvalidAuthorizationResponse)?;
        if callback.scheme() != self.redirect_uri.scheme()
            || callback.host_str() != self.redirect_uri.host_str()
            || callback.port_or_known_default() != self.redirect_uri.port_or_known_default()
            || callback.path() != self.redirect_uri.path()
            || callback.fragment().is_some()
            || !callback.username().is_empty()
            || callback.password().is_some()
        {
            return Err(McpOAuthProtocolError::InvalidAuthorizationResponse);
        }
        let mut values = BTreeMap::new();
        for (name, value) in callback.query_pairs() {
            if !matches!(
                name.as_ref(),
                "code" | "state" | "iss" | "error" | "error_description" | "error_uri"
            ) || values
                .insert(name.into_owned(), value.into_owned())
                .is_some()
            {
                return Err(McpOAuthProtocolError::InvalidAuthorizationResponse);
            }
        }
        let state = values
            .remove("state")
            .ok_or(McpOAuthProtocolError::InvalidAuthorizationResponse)?;
        if !constant_time_eq(state.as_bytes(), self.state.expose_secret().as_bytes()) {
            return Err(McpOAuthProtocolError::InvalidAuthorizationResponse);
        }
        if let Some(issuer) = values.remove("iss")
            && issuer != self.discovery.issuer.as_str()
        {
            return Err(McpOAuthProtocolError::InvalidAuthorizationResponse);
        }
        if values.contains_key("error") {
            return Err(McpOAuthProtocolError::AuthorizationRejected);
        }
        let code = values
            .remove("code")
            .ok_or(McpOAuthProtocolError::InvalidAuthorizationResponse)?;
        let code = validate_token(code)
            .map_err(|_| McpOAuthProtocolError::InvalidAuthorizationResponse)?;
        Ok(McpOAuthAuthorizationCode {
            discovery: self.discovery.clone(),
            client: self.client.clone(),
            scopes: self.scopes.clone(),
            redirect_uri: self.redirect_uri.clone(),
            code: SecretString::new(code),
            verifier: self.verifier.clone(),
        })
    }

    fn consume(&mut self) -> Result<(), McpOAuthProtocolError> {
        if self.consumed {
            return Err(McpOAuthProtocolError::FlowConsumed);
        }
        self.consumed = true;
        if Instant::now() >= self.expires_at {
            return Err(McpOAuthProtocolError::FlowExpired);
        }
        Ok(())
    }

    fn remaining(&self) -> Result<Duration, McpOAuthProtocolError> {
        if self.consumed {
            return Err(McpOAuthProtocolError::FlowConsumed);
        }
        self.expires_at
            .checked_duration_since(Instant::now())
            .ok_or(McpOAuthProtocolError::FlowExpired)
    }

    fn cancel(&mut self) {
        self.consumed = true;
    }
}

/// Owner of a one-shot callback listener bound only to `127.0.0.1` on an ephemeral port.
pub struct McpOAuthLoopbackListener {
    listener: TcpListener,
    redirect_uri: Url,
}

impl fmt::Debug for McpOAuthLoopbackListener {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthLoopbackListener")
            .field("origin", &safe_origin(&self.redirect_uri))
            .finish_non_exhaustive()
    }
}

impl McpOAuthLoopbackListener {
    pub async fn bind() -> Result<Self, McpOAuthProtocolError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| McpOAuthProtocolError::CallbackFailed)?;
        let port = listener
            .local_addr()
            .map_err(|_| McpOAuthProtocolError::CallbackFailed)?
            .port();
        let redirect_uri = Url::parse(&format!("http://127.0.0.1:{port}/callback"))
            .map_err(|_| McpOAuthProtocolError::CallbackFailed)?;
        Ok(Self {
            listener,
            redirect_uri,
        })
    }

    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        self.redirect_uri.as_str()
    }

    pub async fn receive(
        self,
        pending: &mut McpOAuthPendingAuthorization,
    ) -> Result<McpOAuthAuthorizationCode, McpOAuthProtocolError> {
        if pending.redirect_uri != self.redirect_uri {
            pending.cancel();
            return Err(McpOAuthProtocolError::InvalidAuthorizationResponse);
        }
        let timeout = pending.remaining()?;
        let accepted = tokio::time::timeout(timeout, async {
            let (mut socket, _) = self
                .listener
                .accept()
                .await
                .map_err(|_| McpOAuthProtocolError::CallbackFailed)?;
            let target = read_callback_request(&mut socket).await?;
            Ok::<_, McpOAuthProtocolError>((socket, target))
        })
        .await;
        let (mut socket, target) = match accepted {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => {
                pending.cancel();
                return Err(error);
            }
            Err(_) => {
                pending.cancel();
                return Err(McpOAuthProtocolError::FlowExpired);
            }
        };
        let callback = format!(
            "{}{}",
            self.redirect_uri.origin().ascii_serialization(),
            target
        );
        let result = pending.complete_callback(SecretString::new(callback));
        let (status, message) = if result.is_ok() {
            ("200 OK", "Authorization received. You can return to Sigil.")
        } else {
            (
                "400 Bad Request",
                "Authorization could not be accepted. Return to Sigil.",
            )
        };
        let body =
            format!("<!doctype html><meta charset=utf-8><title>Sigil</title><p>{message}</p>");
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
        result
    }
}

/// Opaque, single-use authorization-code exchange input.
pub struct McpOAuthAuthorizationCode {
    discovery: McpOAuthDiscovery,
    client: McpOAuthClientRegistration,
    scopes: Vec<String>,
    redirect_uri: Url,
    code: SecretString,
    verifier: SecretString,
}

impl fmt::Debug for McpOAuthAuthorizationCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthAuthorizationCode")
            .field("scope_count", &self.scopes.len())
            .finish_non_exhaustive()
    }
}

/// Validated token response whose secret values remain in redacted carriers.
pub struct McpOAuthTokenResponse {
    access_token: SecretString,
    refresh_token: Option<SecretString>,
    expires_in_secs: Option<u64>,
    scopes: Vec<String>,
}

impl fmt::Debug for McpOAuthTokenResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthTokenResponse")
            .field("has_access_token", &true)
            .field("has_refresh_token", &self.refresh_token.is_some())
            .field("expires_in_secs", &self.expires_in_secs)
            .field("scope_count", &self.scopes.len())
            .finish()
    }
}

impl McpOAuthTokenResponse {
    #[must_use]
    pub fn access_token(&self) -> &SecretString {
        &self.access_token
    }

    #[must_use]
    pub fn refresh_token(&self) -> Option<&SecretString> {
        self.refresh_token.as_ref()
    }

    #[must_use]
    pub fn expires_in_secs(&self) -> Option<u64> {
        self.expires_in_secs
    }

    #[must_use]
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }
}

#[derive(Debug, Deserialize)]
struct TokenDocument {
    access_token: String,
    token_type: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

/// Exchanges one validated authorization code without redirects or automatic retries.
pub async fn exchange_oauth_authorization_code(
    executor: &dyn McpOAuthHttpExecutor,
    authorization: McpOAuthAuthorizationCode,
) -> Result<McpOAuthTokenResponse, McpOAuthProtocolError> {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("grant_type", "authorization_code");
    serializer.append_pair("code", authorization.code.expose_secret());
    serializer.append_pair("redirect_uri", authorization.redirect_uri.as_str());
    serializer.append_pair("client_id", authorization.client.client_id());
    serializer.append_pair("code_verifier", authorization.verifier.expose_secret());
    serializer.append_pair("resource", authorization.discovery.resource.as_str());
    let mut headers = Vec::new();
    match authorization.client.auth_method {
        ClientAuthMethod::None => {}
        ClientAuthMethod::SecretPost => {
            serializer.append_pair(
                "client_secret",
                authorization
                    .client
                    .client_secret
                    .as_ref()
                    .ok_or(McpOAuthProtocolError::InvalidClient)?
                    .expose_secret(),
            );
        }
        ClientAuthMethod::SecretBasic => {
            let secret = authorization
                .client
                .client_secret
                .as_ref()
                .ok_or(McpOAuthProtocolError::InvalidClient)?;
            let encoded = general_purpose::STANDARD.encode(format!(
                "{}:{}",
                authorization.client.client_id(),
                secret.expose_secret()
            ));
            headers.push((
                "authorization".to_owned(),
                SecretString::new(format!("Basic {encoded}")),
            ));
        }
    };
    let response = executor
        .execute(McpOAuthHttpRequest::post(
            &authorization.discovery.token_endpoint,
            McpOAuthHttpPurpose::TokenExchange,
            "application/x-www-form-urlencoded",
            headers,
            SecretString::new(serializer.finish()),
        ))
        .await
        .map_err(|_| McpOAuthProtocolError::Transport)?;
    if response.status != 200
        || !is_json_content_type(response.content_type.as_deref())
        || response.body.expose_secret().len() > MAX_TOKEN_BODY_BYTES
    {
        return Err(McpOAuthProtocolError::TokenRejected);
    }
    let token: TokenDocument = serde_json::from_str(response.body.expose_secret())
        .map_err(|_| McpOAuthProtocolError::TokenRejected)?;
    if !token.token_type.eq_ignore_ascii_case("bearer") {
        return Err(McpOAuthProtocolError::TokenRejected);
    }
    let access_token = SecretString::new(validate_token(token.access_token)?);
    let refresh_token = token
        .refresh_token
        .map(validate_token)
        .transpose()?
        .map(SecretString::new);
    let scopes = token
        .scope
        .map(|scope| validate_scopes(scope.split(' ').filter(|value| !value.is_empty())))
        .transpose()
        .map_err(|_| McpOAuthProtocolError::TokenRejected)?
        .unwrap_or_else(|| authorization.scopes.clone());
    if scopes.iter().any(|scope| {
        !authorization
            .scopes
            .iter()
            .any(|requested| requested == scope)
    }) {
        return Err(McpOAuthProtocolError::TokenRejected);
    }
    if token
        .expires_in
        .is_some_and(|value| value > MAX_TOKEN_LIFETIME_SECS)
    {
        return Err(McpOAuthProtocolError::TokenRejected);
    }
    Ok(McpOAuthTokenResponse {
        access_token,
        refresh_token,
        expires_in_secs: token.expires_in,
        scopes,
    })
}

async fn fetch_first_json<T: for<'de> Deserialize<'de>>(
    executor: &dyn McpOAuthHttpExecutor,
    candidates: Vec<Url>,
    purpose: McpOAuthHttpPurpose,
    max_body_bytes: usize,
) -> Result<(Url, T), McpOAuthProtocolError> {
    for candidate in candidates {
        let response = executor
            .execute(McpOAuthHttpRequest::get(&candidate, purpose))
            .await
            .map_err(|_| McpOAuthProtocolError::Transport)?;
        if response.status == 404 {
            continue;
        }
        if response.status != 200
            || !is_json_content_type(response.content_type.as_deref())
            || response.body.expose_secret().is_empty()
            || response.body.expose_secret().len() > max_body_bytes
        {
            return Err(McpOAuthProtocolError::InvalidMetadata);
        }
        let document = serde_json::from_str(response.body.expose_secret())
            .map_err(|_| McpOAuthProtocolError::InvalidMetadata)?;
        return Ok((candidate, document));
    }
    Err(McpOAuthProtocolError::MetadataUnavailable)
}

fn protected_resource_metadata_candidates(
    resource: &Url,
) -> Result<Vec<Url>, McpOAuthProtocolError> {
    let inserted = inserted_well_known(resource, "oauth-protected-resource")?;
    let mut root = resource.clone();
    root.set_path("/.well-known/oauth-protected-resource");
    root.set_query(None);
    let mut values = vec![inserted];
    if !values.iter().any(|value| value == &root) {
        values.push(root);
    }
    Ok(values)
}

fn authorization_server_metadata_candidates(
    issuer: &Url,
) -> Result<Vec<Url>, McpOAuthProtocolError> {
    let mut values = vec![
        inserted_well_known(issuer, "oauth-authorization-server")?,
        inserted_well_known(issuer, "openid-configuration")?,
    ];
    let mut appended = issuer.clone();
    let prefix = issuer.path().trim_end_matches('/');
    appended.set_path(&format!("{prefix}/.well-known/openid-configuration"));
    if !values.iter().any(|value| value == &appended) {
        values.push(appended);
    }
    Ok(values)
}

fn inserted_well_known(url: &Url, suffix: &str) -> Result<Url, McpOAuthProtocolError> {
    let mut result = url.clone();
    let path = url.path().trim_start_matches('/').trim_end_matches('/');
    let inserted = if path.is_empty() {
        format!("/.well-known/{suffix}")
    } else {
        format!("/.well-known/{suffix}/{path}")
    };
    result.set_path(&inserted);
    if result.as_str().len() > MAX_ENDPOINT_BYTES {
        return Err(McpOAuthProtocolError::InvalidMetadata);
    }
    Ok(result)
}

fn validate_https_url(value: &str, allow_query: bool) -> Result<Url, ()> {
    if value.is_empty() || value.len() > MAX_ENDPOINT_BYTES || value.chars().any(char::is_control) {
        return Err(());
    }
    let url = Url::parse(value).map_err(|_| ())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (!allow_query && url.query().is_some())
    {
        return Err(());
    }
    Ok(url)
}

fn validate_issuer(value: &str) -> Result<Url, McpOAuthProtocolError> {
    validate_https_url(value, false).map_err(|_| McpOAuthProtocolError::InvalidMetadata)
}

fn validate_loopback_redirect(value: &str) -> Result<Url, McpOAuthProtocolError> {
    let url = Url::parse(value).map_err(|_| McpOAuthProtocolError::InvalidClient)?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || url.path() != "/callback"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(McpOAuthProtocolError::InvalidClient);
    }
    Ok(url)
}

fn validate_client_id(value: &str) -> Result<(), McpOAuthProtocolError> {
    if !valid_bounded_text(value, MAX_CLIENT_ID_BYTES) || value.chars().any(char::is_whitespace) {
        Err(McpOAuthProtocolError::InvalidClient)
    } else {
        Ok(())
    }
}

fn validate_token(value: String) -> Result<String, McpOAuthProtocolError> {
    if value.is_empty()
        || value.len() > MAX_TOKEN_BYTES
        || value
            .bytes()
            .any(|byte| byte == 0 || byte == b'\r' || byte == b'\n' || byte < 0x20)
    {
        Err(McpOAuthProtocolError::TokenRejected)
    } else {
        Ok(value)
    }
}

fn validate_scopes<'a>(
    values: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<String>, McpOAuthProtocolError> {
    let mut scopes = Vec::new();
    let mut unique = BTreeSet::new();
    let mut total = 0usize;
    for value in values {
        let valid = !value.is_empty()
            && value.len() <= MAX_SCOPE_BYTES
            && value.bytes().all(|byte| {
                byte == 0x21 || (0x23..=0x5b).contains(&byte) || (0x5d..=0x7e).contains(&byte)
            });
        total = total.saturating_add(value.len());
        if !valid
            || total > MAX_SCOPE_TOTAL_BYTES
            || scopes.len() >= MAX_SCOPE_COUNT
            || !unique.insert(value.to_owned())
        {
            return Err(McpOAuthProtocolError::InvalidClient);
        }
        scopes.push(value.to_owned());
    }
    Ok(scopes)
}

fn valid_bounded_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn valid_metadata_list(values: &[String]) -> bool {
    if values.is_empty() || values.len() > MAX_METADATA_LIST_ITEMS {
        return false;
    }
    let mut unique = BTreeSet::new();
    values
        .iter()
        .all(|value| valid_bounded_text(value, 512) && unique.insert(value.as_str()))
}

fn is_json_content_type(value: Option<&str>) -> bool {
    value
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
}

fn random_base64url_32() -> String {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn safe_origin(url: &Url) -> String {
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or("invalid"),
        url.port_or_known_default().unwrap_or_default()
    )
}

async fn read_callback_request(
    socket: &mut tokio::net::TcpStream,
) -> Result<String, McpOAuthProtocolError> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let read = socket
            .read(&mut buffer)
            .await
            .map_err(|_| McpOAuthProtocolError::CallbackFailed)?;
        if read == 0 {
            return Err(McpOAuthProtocolError::InvalidAuthorizationResponse);
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > MAX_CALLBACK_REQUEST_BYTES {
            return Err(McpOAuthProtocolError::InvalidAuthorizationResponse);
        }
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = std::str::from_utf8(&bytes)
        .map_err(|_| McpOAuthProtocolError::InvalidAuthorizationResponse)?;
    let request_line = request
        .split("\r\n")
        .next()
        .ok_or(McpOAuthProtocolError::InvalidAuthorizationResponse)?;
    let mut parts = request_line.split(' ');
    let method = parts.next();
    let target = parts.next();
    let version = parts.next();
    if method != Some("GET")
        || !matches!(version, Some("HTTP/1.0" | "HTTP/1.1"))
        || parts.next().is_some()
    {
        return Err(McpOAuthProtocolError::InvalidAuthorizationResponse);
    }
    let target = target.ok_or(McpOAuthProtocolError::InvalidAuthorizationResponse)?;
    if !target.starts_with("/callback?")
        || target.len() > MAX_CALLBACK_URL_BYTES
        || target.chars().any(char::is_control)
    {
        return Err(McpOAuthProtocolError::InvalidAuthorizationResponse);
    }
    Ok(target.to_owned())
}

#[cfg(test)]
#[path = "../tests/oauth_tests.rs"]
mod tests;
