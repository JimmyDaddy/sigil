use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    net::SocketAddr,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::StreamExt;
use hmac::{Hmac, Mac};
use regex::Regex;
use reqwest::{
    Client, Method, Proxy, StatusCode,
    header::{
        ACCEPT, AUTHORIZATION, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderName,
        HeaderValue, WWW_AUTHENTICATE,
    },
    redirect::Policy,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::Sha256;
use sigil_kernel::SecretString;
use sigil_kernel::{WebBudgetByteKind, WebBudgetReservation};
use thiserror::Error;
use tokio::sync::Mutex;
use url::Url;
use uuid::Uuid;

const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";
const PREVIOUS_PROTOCOL_VERSION: &str = "2025-06-18";
const MCP_SESSION_HEADER: &str = "mcp-session-id";
const MCP_VERSION_HEADER: &str = "mcp-protocol-version";
const MAX_CUSTOM_HEADERS: usize = 32;
const MAX_HEADER_NAME_BYTES: usize = 128;
const MAX_HEADER_VALUE_BYTES: usize = 8 * 1024;
const MAX_HEADER_TOTAL_BYTES: usize = 32 * 1024;
const MAX_SESSION_ID_BYTES: usize = 1024;
const MAX_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_SCHEMA_DEPTH: usize = 32;
const MAX_SCHEMA_NODES: usize = 4096;
const MAX_SCHEMA_PROPERTIES: usize = 512;
const MAX_FORM_SCHEMA_BYTES: usize = 32 * 1024;
const MAX_FORM_SCHEMA_DEPTH: usize = 8;
const MAX_FORM_SCHEMA_NODES: usize = 512;
const MAX_FORM_PROPERTIES: usize = 32;
const MAX_SCHEMA_TEXT_BYTES: usize = 1024;
const MAX_FORM_MESSAGE_BYTES: usize = 4 * 1024;
const MAX_FORM_RESPONSE_BYTES: usize = 16 * 1024;
const MAX_FORM_STRING_BYTES: usize = 4 * 1024;
const MAX_PAGES: usize = 32;
const MAX_TOOLS: usize = 512;
const MAX_AUTH_CHALLENGE_BYTES: usize = 4 * 1024;

static HEADER_FINGERPRINT_KEY: OnceLock<[u8; 32]> = OnceLock::new();

fn validate_fingerprint(value: &str) -> Result<(), McpStreamableHttpError> {
    if value.is_empty()
        || value.len() > 1024
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        Err(McpStreamableHttpError::InvalidDialPlan)
    } else {
        Ok(())
    }
}

mod auth;
mod elicitation;
mod framing;
mod lifecycle;
mod schema;
mod tools;
mod transport;
use auth::{normalize_status, resolve_headers, validate_session_header};
pub use elicitation::{McpRemoteFormField, McpRemoteFormFieldKind, ValidatedMcpFormRequest};
use framing::{
    matches_content_type, parse_sse_messages, parse_sse_response, read_bounded_body, rpc_result,
    single_header, validate_response_envelope,
};
pub use lifecycle::McpStreamableHttpClient;
pub(super) use lifecycle::RpcResponse;
pub use schema::CompiledMcpSchema;
pub use tools::{McpCallToolResult, McpRemoteTool};
use transport::{build_client, safe_origin, validate_endpoint, validate_safe_destination};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpRemoteProtocolVersion {
    V2025_11_25,
    V2025_06_18,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRemoteServerIdentity {
    pub name: String,
    pub version: String,
    pub fingerprint: String,
}

/// Synchronous first-body-poll barrier used by runtime-owned durable query accounting.
pub trait McpRequestBodyObserver: Send + Sync {
    fn on_first_body_poll(&self) -> Result<(), McpStreamableHttpError>;
}

impl McpRemoteProtocolVersion {
    fn parse(value: &str) -> Result<Self, McpStreamableHttpError> {
        match value {
            LATEST_PROTOCOL_VERSION => Ok(Self::V2025_11_25),
            PREVIOUS_PROTOCOL_VERSION => Ok(Self::V2025_06_18),
            _ => Err(McpStreamableHttpError::UnsupportedProtocolVersion),
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::V2025_11_25 => LATEST_PROTOCOL_VERSION,
            Self::V2025_06_18 => PREVIOUS_PROTOCOL_VERSION,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpStreamableHttpLifecycle {
    Disconnected,
    Initializing,
    InitializedNotificationPending,
    Ready,
    Closing,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpStreamableHttpRouteEvidence {
    DirectAllAddressesPinned,
    ProxyRemoteLogicalGuardOnly,
}

#[derive(Clone)]
pub enum McpStreamableHttpRoute {
    Direct { addresses: Vec<SocketAddr> },
    EnvironmentProxy { proxy_url: SecretString },
}

impl fmt::Debug for McpStreamableHttpRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct { addresses } => formatter
                .debug_struct("Direct")
                .field("pinned_address_count", &addresses.len())
                .finish(),
            Self::EnvironmentProxy { .. } => formatter.write_str("EnvironmentProxy([redacted])"),
        }
    }
}

/// Secret-bearing, runtime-authorized dial plan consumed once by the remote protocol core.
///
/// Runtime must construct this only after the durable transport authorization and disclosure
/// barrier. `Debug` exposes only safe destinations and evidence, never URL query or credentials.
pub struct McpStreamableHttpAuthorizedDialPlan {
    endpoint: SecretString,
    safe_logical_destination: String,
    safe_transport_destination: String,
    route: McpStreamableHttpRoute,
    evidence: McpStreamableHttpRouteEvidence,
    profile_config_proxy_fingerprint: String,
    live_header_fingerprint: String,
    budget: Option<WebBudgetReservation>,
}

impl fmt::Debug for McpStreamableHttpAuthorizedDialPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpStreamableHttpAuthorizedDialPlan")
            .field("safe_logical_destination", &self.safe_logical_destination)
            .field(
                "safe_transport_destination",
                &self.safe_transport_destination,
            )
            .field("route", &self.route)
            .field("evidence", &self.evidence)
            .field(
                "profile_config_proxy_fingerprint",
                &self.profile_config_proxy_fingerprint,
            )
            .field("live_header_fingerprint", &self.live_header_fingerprint)
            .finish()
    }
}

impl McpStreamableHttpAuthorizedDialPlan {
    pub fn direct(
        endpoint: SecretString,
        safe_logical_destination: impl Into<String>,
        addresses: Vec<SocketAddr>,
        profile_config_proxy_fingerprint: impl Into<String>,
        live_header_fingerprint: impl Into<String>,
        budget: WebBudgetReservation,
    ) -> Result<Self, McpStreamableHttpError> {
        if addresses.is_empty() {
            return Err(McpStreamableHttpError::InvalidDialPlan);
        }
        let safe_logical_destination = safe_logical_destination.into();
        validate_safe_destination(&safe_logical_destination)?;
        validate_endpoint(endpoint.expose_secret())?;
        let profile_config_proxy_fingerprint = profile_config_proxy_fingerprint.into();
        let live_header_fingerprint = live_header_fingerprint.into();
        validate_fingerprint(&profile_config_proxy_fingerprint)?;
        validate_fingerprint(&live_header_fingerprint)?;
        Ok(Self {
            endpoint,
            safe_transport_destination: safe_logical_destination.clone(),
            safe_logical_destination,
            route: McpStreamableHttpRoute::Direct { addresses },
            evidence: McpStreamableHttpRouteEvidence::DirectAllAddressesPinned,
            profile_config_proxy_fingerprint,
            live_header_fingerprint,
            budget: Some(budget),
        })
    }

    pub fn environment_proxy(
        endpoint: SecretString,
        safe_logical_destination: impl Into<String>,
        safe_transport_destination: impl Into<String>,
        proxy_url: SecretString,
        profile_config_proxy_fingerprint: impl Into<String>,
        live_header_fingerprint: impl Into<String>,
        budget: WebBudgetReservation,
    ) -> Result<Self, McpStreamableHttpError> {
        let safe_logical_destination = safe_logical_destination.into();
        let safe_transport_destination = safe_transport_destination.into();
        validate_safe_destination(&safe_logical_destination)?;
        validate_safe_destination(&safe_transport_destination)?;
        validate_endpoint(endpoint.expose_secret())?;
        let proxy = Url::parse(proxy_url.expose_secret())
            .map_err(|_| McpStreamableHttpError::InvalidDialPlan)?;
        if !matches!(proxy.scheme(), "http" | "https") || proxy.host_str().is_none() {
            return Err(McpStreamableHttpError::InvalidDialPlan);
        }
        let profile_config_proxy_fingerprint = profile_config_proxy_fingerprint.into();
        let live_header_fingerprint = live_header_fingerprint.into();
        validate_fingerprint(&profile_config_proxy_fingerprint)?;
        validate_fingerprint(&live_header_fingerprint)?;
        Ok(Self {
            endpoint,
            safe_logical_destination,
            safe_transport_destination,
            route: McpStreamableHttpRoute::EnvironmentProxy { proxy_url },
            evidence: McpStreamableHttpRouteEvidence::ProxyRemoteLogicalGuardOnly,
            profile_config_proxy_fingerprint,
            live_header_fingerprint,
            budget: Some(budget),
        })
    }

    #[must_use]
    pub fn safe_logical_destination(&self) -> &str {
        &self.safe_logical_destination
    }

    #[must_use]
    pub fn safe_transport_destination(&self) -> &str {
        &self.safe_transport_destination
    }

    #[must_use]
    pub fn evidence(&self) -> McpStreamableHttpRouteEvidence {
        self.evidence
    }

    fn take_budget(&mut self) -> Result<WebBudgetReservation, McpStreamableHttpError> {
        self.budget
            .take()
            .ok_or(McpStreamableHttpError::InvalidDialPlan)
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum McpStreamableHttpDestinationError {
    #[error("remote MCP transport authorization or disclosure failed")]
    PreEgressRejected,
    #[error("remote MCP destination guard rejected the destination")]
    DestinationRejected,
    #[error("remote MCP task-tree network budget is exhausted")]
    BudgetExhausted,
}

#[async_trait]
pub trait McpStreamableHttpDestinationAuthorizer: Send + Sync {
    /// Returns the configured endpoint without authorization, DNS or other external effects.
    fn endpoint(&self) -> SecretString;

    /// Safe runtime binding for endpoint/transport identity. Implementations may override with a
    /// stronger composite binding; the default reuses the already secret-safe profile binding.
    fn transport_fingerprint(&self) -> String {
        self.profile_config_proxy_fingerprint()
    }

    /// Returns the exact non-secret config/proxy binding expected on every attempt.
    fn profile_config_proxy_fingerprint(&self) -> String;

    /// Returns the process-local HMAC binding of resolved header values.
    fn live_header_fingerprint(&self) -> String;

    /// Completes authorization, durable disclosure and shared destination guarding before return.
    async fn authorize_destination(
        &self,
    ) -> Result<McpStreamableHttpAuthorizedDialPlan, McpStreamableHttpDestinationError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpStreamableHttpLimits {
    pub max_header_bytes: usize,
    pub max_body_bytes: usize,
    pub max_sse_line_bytes: usize,
    pub max_sse_event_bytes: usize,
    pub max_sse_events: usize,
    pub response_timeout: Duration,
}

impl Default for McpStreamableHttpLimits {
    fn default() -> Self {
        Self {
            max_header_bytes: 32 * 1024,
            max_body_bytes: 8 * 1024 * 1024,
            max_sse_line_bytes: 64 * 1024,
            max_sse_event_bytes: 1024 * 1024,
            max_sse_events: 256,
            response_timeout: Duration::from_secs(30),
        }
    }
}

impl McpStreamableHttpLimits {
    fn validate(self) -> Result<Self, McpStreamableHttpError> {
        if self.max_header_bytes == 0
            || self.max_body_bytes == 0
            || self.max_sse_line_bytes == 0
            || self.max_sse_event_bytes == 0
            || self.max_sse_events == 0
            || self.response_timeout.is_zero()
        {
            return Err(McpStreamableHttpError::InvalidLimits);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpRemoteClientCapabilities {
    pub roots: bool,
    pub form_elicitation: bool,
}

/// Safe, pre-validated root exposed only when the remote server explicitly negotiated roots.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct McpRemoteRoot {
    pub uri: String,
    pub name: String,
}

impl McpRemoteRoot {
    pub fn new(
        uri: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<Self, McpStreamableHttpError> {
        let uri = uri.into();
        let name = name.into();
        let parsed = Url::parse(&uri).map_err(|_| McpStreamableHttpError::ConfigurationInvalid)?;
        if parsed.scheme() != "file"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || uri.len() > 8 * 1024
            || name.is_empty()
            || name.len() > 512
            || name.chars().any(char::is_control)
        {
            return Err(McpStreamableHttpError::ConfigurationInvalid);
        }
        Ok(Self { uri, name })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum McpRemoteFormResponse {
    Accept(Value),
    Decline,
    Cancel,
}

#[async_trait]
pub trait McpRemoteFormHandler: Send + Sync {
    async fn handle_form(
        &self,
        request: ValidatedMcpFormRequest,
    ) -> Result<McpRemoteFormResponse, McpStreamableHttpError>;
}

impl McpRemoteClientCapabilities {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    fn wire(&self, version: McpRemoteProtocolVersion) -> Value {
        let mut capabilities = Map::new();
        if self.roots {
            capabilities.insert("roots".to_owned(), json!({ "listChanged": true }));
        }
        if self.form_elicitation {
            capabilities.insert(
                "elicitation".to_owned(),
                match version {
                    McpRemoteProtocolVersion::V2025_11_25 => json!({ "form": {} }),
                    McpRemoteProtocolVersion::V2025_06_18 => json!({}),
                },
            );
        }
        Value::Object(capabilities)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpStreamableHttpHeaderConfig {
    pub literal: BTreeMap<String, String>,
    pub from_env: BTreeMap<String, String>,
    pub bearer_token_env_var: Option<String>,
}

pub trait McpStreamableHttpHeaderEnvironment: Send + Sync {
    fn resolve(&self, name: &str) -> Option<SecretString>;
}

#[derive(Clone)]
struct ResolvedHeaders {
    values: Vec<(HeaderName, SecretString)>,
    has_static_credential: bool,
    live_fingerprint: String,
}

/// Non-serializable activation result that owns resolved header secrets and their live HMAC.
/// Construct this before permission/disclosure/DNS, then bind its fingerprint into the runtime
/// authorizer and every authorized dial plan.
pub struct PreparedMcpStreamableHttpHeaders {
    endpoint_secret: SecretString,
    endpoint: Url,
    headers: ResolvedHeaders,
}

impl fmt::Debug for PreparedMcpStreamableHttpHeaders {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedMcpStreamableHttpHeaders")
            .field("safe_endpoint", &safe_origin(&self.endpoint))
            .field("headers", &self.headers)
            .finish()
    }
}

impl PreparedMcpStreamableHttpHeaders {
    pub fn prepare(
        endpoint: SecretString,
        config: &McpStreamableHttpHeaderConfig,
        environment: &dyn McpStreamableHttpHeaderEnvironment,
    ) -> Result<Self, McpStreamableHttpError> {
        let parsed = validate_endpoint(endpoint.expose_secret())?;
        let headers = resolve_headers(config, environment, &parsed)?;
        Ok(Self {
            endpoint_secret: endpoint,
            endpoint: parsed,
            headers,
        })
    }

    #[must_use]
    pub fn live_header_fingerprint(&self) -> &str {
        &self.headers.live_fingerprint
    }

    #[must_use]
    pub fn endpoint(&self) -> SecretString {
        self.endpoint_secret.clone()
    }
}

impl fmt::Debug for ResolvedHeaders {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedHeaders")
            .field("header_count", &self.values.len())
            .field("has_static_credential", &self.has_static_credential)
            .field("live_fingerprint", &self.live_fingerprint)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpStreamableHttpAuthState {
    Anonymous,
    StaticCredential,
}

#[derive(Debug, Error)]
pub enum McpStreamableHttpError {
    #[error("remote MCP destination authorization failed")]
    DestinationAuthorization(#[from] McpStreamableHttpDestinationError),
    #[error("remote MCP endpoint is invalid")]
    InvalidEndpoint,
    #[error("remote MCP authorized dial plan is invalid")]
    InvalidDialPlan,
    #[error("remote MCP safe destination projection is invalid")]
    InvalidSafeDestination,
    #[error("remote MCP limits must be non-zero")]
    InvalidLimits,
    #[error("remote MCP task-tree network budget is exhausted")]
    BudgetExhausted,
    #[error("remote MCP lifecycle does not allow this operation")]
    InvalidLifecycle,
    #[error("remote MCP server selected an unsupported protocol version")]
    UnsupportedProtocolVersion,
    #[error("remote MCP initialized notification was rejected")]
    InitializedNotificationRejected,
    #[error("remote MCP response has unexpected HTTP status {status}")]
    UnexpectedHttpStatus { status: u16 },
    #[error("remote MCP response has an unexpected content type")]
    UnexpectedContentType,
    #[error("remote MCP response headers exceed the bounded limit")]
    HeaderLimitExceeded,
    #[error("remote MCP response body exceeds the bounded limit")]
    BodyLimitExceeded,
    #[error("remote MCP SSE response exceeds a framing limit")]
    SseLimitExceeded,
    #[error("remote MCP response contains malformed JSON-RPC")]
    MalformedEnvelope,
    #[error("remote MCP tool result is missing required content")]
    MissingRequiredContent,
    #[error("remote MCP response id is missing, duplicated or mismatched")]
    ResponseIdMismatch,
    #[error("remote MCP server did not advertise tools capability")]
    MissingToolsCapability,
    #[error("remote MCP session id is invalid")]
    InvalidSessionId,
    #[error("remote MCP session expired")]
    SessionExpired,
    #[error("remote MCP tools/list pagination is invalid or exceeds limits")]
    InvalidPagination,
    #[error("remote MCP schema is invalid, unsupported or exceeds limits")]
    SchemaDrift,
    #[error("remote MCP elicitation form is invalid, unsupported or exceeds limits")]
    InvalidForm,
    #[error("remote MCP URL elicitation is unsupported")]
    UrlElicitationUnsupported,
    #[error("remote MCP server method is not negotiated")]
    CapabilityNotNegotiated,
    #[error("remote MCP custom header configuration is invalid")]
    ConfigurationInvalid,
    #[error("remote MCP authentication is required")]
    AuthenticationRequired,
    #[error("remote MCP static credential was rejected")]
    AuthenticationFailed,
    #[error("remote MCP OAuth is unsupported")]
    OAuthUnsupported,
    #[error("remote MCP authentication challenge is invalid or exceeds limits")]
    InvalidAuthenticationChallenge,
    #[error("remote MCP access was denied")]
    AccessDenied,
    #[error("remote MCP server rate limit was reached")]
    RateLimited,
    #[error("remote MCP service is unavailable")]
    ServiceUnavailable,
    #[error("remote MCP request timed out")]
    Timeout,
    #[error("remote MCP request was cancelled")]
    Cancelled,
    #[error("remote MCP transport failed")]
    Transport,
    #[error("remote MCP JSON-RPC error {code}")]
    JsonRpcError { code: i64 },
}

#[cfg(test)]
#[path = "tests/remote_elicitation_tests.rs"]
mod remote_elicitation_tests;
#[cfg(test)]
#[path = "tests/schema_validation_tests.rs"]
mod schema_validation_tests;
#[cfg(test)]
#[path = "tests/streamable_http_tests.rs"]
mod streamable_http_tests;
