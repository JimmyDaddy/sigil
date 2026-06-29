use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sigil_kernel::{PublicRunEvent, PublicRunEventKind, ToolApprovalUserDecision};
use thiserror::Error as ThisError;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::broadcast,
};

/// Environment variable read by the HTTP adapter for its bearer token by default.
pub const DEFAULT_HTTP_TOKEN_ENV: &str = "SIGIL_HTTP_TOKEN";
/// SSE event name used for public run events.
pub const HTTP_RUN_EVENT_SSE_NAME: &str = "run_event";
/// Current schema version for HTTP protocol event envelopes.
pub const HTTP_PROTOCOL_EVENT_SCHEMA_VERSION: u32 = 1;
/// Current protocol command/event surface version.
pub const HTTP_PROTOCOL_VERSION: u16 = 1;
/// OpenAPI version emitted for the MVP desktop/app-server command surface.
pub const HTTP_OPENAPI_VERSION: &str = "3.1.0";

const HTTP_PROTOCOL_CURSOR_PREFIX: &str = "sigil-http-run-v1";
const HTTP_MAX_HEADER_BYTES: usize = 64 * 1024;
const HTTP_MAX_BODY_BYTES: usize = 1024 * 1024;

/// Versioned command envelope shared by future HTTP, IDE, and TUI command bridges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpCommandEnvelope<T> {
    pub protocol_version: u16,
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_stream_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub payload: T,
}

impl<T> HttpCommandEnvelope<T> {
    /// Creates a command envelope using the current HTTP protocol version.
    #[must_use]
    pub fn new(
        command_id: impl Into<String>,
        client_id: impl Into<String>,
        session_id: impl Into<String>,
        payload: T,
    ) -> Self {
        Self {
            protocol_version: HTTP_PROTOCOL_VERSION,
            command_id: command_id.into(),
            client_id: client_id.into(),
            session_id: session_id.into(),
            expected_stream_sequence: None,
            correlation_id: None,
            payload,
        }
    }

    /// Adds an optimistic stream-sequence guard for stale-client protection.
    #[must_use]
    pub fn with_expected_stream_sequence(mut self, sequence: u64) -> Self {
        self.expected_stream_sequence = Some(sequence);
        self
    }

    /// Adds a durable-event correlation id.
    #[must_use]
    pub fn with_correlation_id(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }

    /// Fails closed when a client sends an unsupported command envelope version.
    ///
    /// # Errors
    ///
    /// Returns an error when `protocol_version` does not match the current supported version.
    pub fn ensure_supported(&self) -> Result<(), HttpProtocolVersionError> {
        if self.protocol_version != HTTP_PROTOCOL_VERSION {
            return Err(HttpProtocolVersionError::Unsupported {
                supported: HTTP_PROTOCOL_VERSION,
                received: self.protocol_version,
            });
        }
        Ok(())
    }
}

/// Returns the MVP OpenAPI description for the local HTTP command surface.
///
/// The document intentionally covers only routes implemented by this crate:
/// health, session creation, run start, and approval decision submission.
#[must_use]
pub fn http_openapi_document() -> Value {
    json!({
        "openapi": HTTP_OPENAPI_VERSION,
        "info": {
            "title": "Sigil Local App Server API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Localhost-only adapter surface for desktop and future local clients."
        },
        "security": [{ "BearerAuth": [] }],
        "paths": {
            "/health": {
                "get": {
                    "summary": "Local listener health check",
                    "security": [],
                    "responses": {
                        "200": {
                            "description": "Listener is running",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/HealthResponse" }
                                }
                            }
                        }
                    }
                }
            },
            "/sessions": {
                "post": {
                    "summary": "Create a local session handle",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/SessionCreateRequest" }
                            }
                        }
                    },
                    "responses": {
                        "201": {
                            "description": "Session snapshot",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/SessionSnapshot" }
                                }
                            }
                        },
                        "401": { "$ref": "#/components/responses/Unauthorized" }
                    }
                }
            },
            "/sessions/{session_id}/runs": {
                "post": {
                    "summary": "Start a run in a session",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/RunStartCommand" }
                            }
                        }
                    },
                    "responses": {
                        "201": {
                            "description": "Run-start command receipt",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/RunStartCommandReceipt" }
                                }
                            }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" }
                    }
                }
            },
            "/runs/{run_id}/approvals/{call_id}": {
                "post": {
                    "summary": "Submit an approval decision for a pending tool call",
                    "parameters": [
                        { "$ref": "#/components/parameters/RunId" },
                        { "$ref": "#/components/parameters/CallId" }
                    ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/ApprovalDecisionCommand" }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Approval command receipt",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/ApprovalCommandReceipt" }
                                }
                            }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" }
                    }
                }
            }
        },
        "components": {
            "securitySchemes": {
                "BearerAuth": {
                    "type": "http",
                    "scheme": "bearer"
                }
            },
            "parameters": {
                "SessionId": {
                    "name": "session_id",
                    "in": "path",
                    "required": true,
                    "schema": { "type": "string" }
                },
                "RunId": {
                    "name": "run_id",
                    "in": "path",
                    "required": true,
                    "schema": { "type": "string" }
                },
                "CallId": {
                    "name": "call_id",
                    "in": "path",
                    "required": true,
                    "schema": { "type": "string" }
                }
            },
            "responses": {
                "BadRequest": { "description": "Invalid request body or command payload", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                "Unauthorized": { "description": "Bearer token is missing or invalid", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                "NotFound": { "description": "Session, run, or route was not found", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                "Conflict": { "description": "Command is stale, mismatched, expired, or not pending", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
            },
            "schemas": {
                "HealthResponse": {
                    "type": "object",
                    "required": ["status"],
                    "properties": { "status": { "type": "string", "const": "ok" } }
                },
                "SessionCreateRequest": {
                    "type": "object",
                    "properties": {
                        "label": { "type": "string" }
                    }
                },
                "SessionSnapshot": {
                    "type": "object",
                    "required": ["id", "label", "created_at_ms", "run_ids"],
                    "properties": {
                        "id": { "type": "string" },
                        "label": { "type": ["string", "null"] },
                        "created_at_ms": { "type": "integer", "format": "uint64" },
                        "run_ids": { "type": "array", "items": { "type": "string" } }
                    }
                },
                "CommandEnvelopeBase": {
                    "type": "object",
                    "required": ["protocol_version", "command_id", "client_id", "session_id", "payload"],
                    "properties": {
                        "protocol_version": { "type": "integer", "const": HTTP_PROTOCOL_VERSION },
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "expected_stream_sequence": { "type": ["integer", "null"], "format": "uint64" },
                        "correlation_id": { "type": ["string", "null"] }
                    }
                },
                "RunStartCommand": {
                    "allOf": [
                        { "$ref": "#/components/schemas/CommandEnvelopeBase" },
                        {
                            "type": "object",
                            "required": ["payload"],
                            "properties": {
                                "payload": { "$ref": "#/components/schemas/RunStartRequest" }
                            }
                        }
                    ]
                },
                "RunStartRequest": {
                    "type": "object",
                    "required": ["prompt", "approval_mode"],
                    "properties": {
                        "prompt": { "type": "string" },
                        "approval_mode": { "$ref": "#/components/schemas/RunApprovalMode" }
                    }
                },
                "RunApprovalMode": {
                    "type": "string",
                    "enum": ["ask", "allow_readonly", "deny"]
                },
                "RunStartCommandReceipt": {
                    "type": "object",
                    "required": ["command_id", "client_id", "session_id", "run", "replayed"],
                    "properties": {
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "expected_stream_sequence": { "type": ["integer", "null"], "format": "uint64" },
                        "correlation_id": { "type": ["string", "null"] },
                        "run": { "$ref": "#/components/schemas/RunSnapshot" },
                        "replayed": { "type": "boolean" }
                    }
                },
                "RunSnapshot": {
                    "type": "object",
                    "required": ["id", "session_id", "status", "prompt", "approval_mode", "created_at_ms", "updated_at_ms", "stream_sequence", "pending_approval_call_ids"],
                    "properties": {
                        "id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "status": { "$ref": "#/components/schemas/RunStatus" },
                        "prompt": { "type": "string" },
                        "approval_mode": { "$ref": "#/components/schemas/RunApprovalMode" },
                        "created_at_ms": { "type": "integer", "format": "uint64" },
                        "updated_at_ms": { "type": "integer", "format": "uint64" },
                        "stream_sequence": { "type": "integer", "format": "uint64" },
                        "pending_approval_call_ids": { "type": "array", "items": { "type": "string" } }
                    }
                },
                "RunStatus": {
                    "type": "string",
                    "enum": ["starting", "running", "waiting_for_approval", "completed", "cancelled", "failed"]
                },
                "ApprovalDecisionCommand": {
                    "allOf": [
                        { "$ref": "#/components/schemas/CommandEnvelopeBase" },
                        {
                            "type": "object",
                            "required": ["payload"],
                            "properties": {
                                "payload": { "$ref": "#/components/schemas/ApprovalDecisionRequest" }
                            }
                        }
                    ]
                },
                "ApprovalDecisionRequest": {
                    "type": "object",
                    "required": ["approval_request_id", "tool_call_hash", "policy_version", "expires_at_ms", "decision"],
                    "properties": {
                        "approval_request_id": { "type": "string" },
                        "tool_call_hash": { "type": "string" },
                        "policy_version": { "type": "string" },
                        "expires_at_ms": { "type": "integer", "format": "uint64" },
                        "decision": { "$ref": "#/components/schemas/ApprovalDecision" },
                        "reason": { "type": ["string", "null"] }
                    }
                },
                "ApprovalDecision": {
                    "type": "string",
                    "enum": ["approve", "deny"]
                },
                "ApprovalCommandReceipt": {
                    "type": "object",
                    "required": ["command_id", "client_id", "session_id", "run_id", "call_id", "decision", "replayed"],
                    "properties": {
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "run_id": { "type": "string" },
                        "call_id": { "type": "string" },
                        "expected_stream_sequence": { "type": ["integer", "null"], "format": "uint64" },
                        "correlation_id": { "type": ["string", "null"] },
                        "decision": { "$ref": "#/components/schemas/ApprovalDecisionRecord" },
                        "replayed": { "type": "boolean" }
                    }
                },
                "ApprovalDecisionRecord": {
                    "type": "object",
                    "required": ["run_id", "call_id", "decision"],
                    "properties": {
                        "run_id": { "type": "string" },
                        "call_id": { "type": "string" },
                        "decision": { "type": "string", "enum": ["approved", "denied"] },
                        "reason": { "type": ["string", "null"] }
                    }
                },
                "ErrorResponse": {
                    "type": "object",
                    "required": ["error"],
                    "properties": {
                        "error": {
                            "type": "object",
                            "required": ["code", "message"],
                            "properties": {
                                "code": { "type": "string" },
                                "message": { "type": "string" }
                            }
                        }
                    }
                }
            }
        }
    })
}

/// Protocol-version errors for command DTOs.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpProtocolVersionError {
    /// Client command uses another protocol version.
    #[error("unsupported http protocol version {received}; supported version is {supported}")]
    Unsupported { supported: u16, received: u16 },
}

/// Configuration for the local HTTP/SSE adapter.
///
/// This crate is intentionally transport-thin: it owns HTTP-facing DTOs and will
/// delegate agent execution to `sigil-runtime` and shared contracts from `sigil-kernel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct HttpServerConfig {
    /// Interface address the server should bind to.
    pub bind_host: IpAddr,
    /// TCP port to bind. `0` lets the operating system choose an available local port.
    pub port: u16,
    /// Authentication controls for HTTP clients.
    pub auth: HttpAuthConfig,
}

impl HttpServerConfig {
    /// Returns the configured bind address.
    #[must_use]
    pub fn bind_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind_host, self.port)
    }

    /// Returns whether the adapter is configured to accept only loopback traffic.
    #[must_use]
    pub fn is_loopback_only(&self) -> bool {
        self.bind_host.is_loopback()
    }

    /// Returns whether bearer-token authentication is required.
    #[must_use]
    pub fn token_required(&self) -> bool {
        self.auth.require_token
    }

    /// Validates safety invariants that are independent from any concrete HTTP framework.
    ///
    /// # Errors
    ///
    /// Returns an error when token auth is required but has no environment variable,
    /// or when a non-loopback bind disables token auth.
    pub fn validate(&self) -> Result<(), HttpServerConfigError> {
        if self.auth.require_token && self.auth.token_env.trim().is_empty() {
            return Err(HttpServerConfigError::MissingTokenEnv);
        }
        if !self.is_loopback_only() && !self.auth.require_token {
            return Err(HttpServerConfigError::ExternalBindWithoutToken);
        }
        Ok(())
    }
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        Self {
            bind_host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            auth: HttpAuthConfig::default(),
        }
    }
}

/// Authentication controls for the HTTP/SSE adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct HttpAuthConfig {
    /// Require clients to send a bearer token.
    pub require_token: bool,
    /// Environment variable containing the bearer token.
    pub token_env: String,
}

impl Default for HttpAuthConfig {
    fn default() -> Self {
        Self {
            require_token: true,
            token_env: DEFAULT_HTTP_TOKEN_ENV.to_owned(),
        }
    }
}

impl HttpAuthConfig {
    /// Builds a bearer-token validator from an already resolved token value.
    ///
    /// # Errors
    ///
    /// Returns an error when token auth is required but no non-empty token was provided.
    pub fn validator_from_token(
        &self,
        token: Option<&str>,
    ) -> Result<HttpAuthValidator, HttpAuthError> {
        if !self.require_token {
            return Ok(HttpAuthValidator::disabled());
        }
        let Some(token) = token.map(str::trim).filter(|value| !value.is_empty()) else {
            return Err(HttpAuthError::MissingToken {
                token_env: self.token_env.clone(),
            });
        };
        Ok(HttpAuthValidator::required(token))
    }
}

/// Configuration validation errors for the HTTP/SSE adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpServerConfigError {
    /// Token auth is enabled but no environment variable name was configured.
    MissingTokenEnv,
    /// A non-loopback bind address cannot disable token auth.
    ExternalBindWithoutToken,
}

impl fmt::Display for HttpServerConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTokenEnv => {
                write!(
                    f,
                    "http auth token env must be set when token auth is required"
                )
            }
            Self::ExternalBindWithoutToken => {
                write!(
                    f,
                    "http token auth is required for non-loopback bind addresses"
                )
            }
        }
    }
}

impl Error for HttpServerConfigError {}

/// Bearer-token validator for the HTTP adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpAuthValidator {
    expected_token: Option<String>,
}

impl HttpAuthValidator {
    /// Creates a validator that accepts requests without an Authorization header.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            expected_token: None,
        }
    }

    /// Creates a validator that requires `Bearer <token>`.
    #[must_use]
    fn required(token: impl Into<String>) -> Self {
        Self {
            expected_token: Some(token.into()),
        }
    }

    /// Returns whether requests must present a bearer token.
    #[must_use]
    pub fn token_required(&self) -> bool {
        self.expected_token.is_some()
    }

    /// Validates one raw Authorization header value.
    ///
    /// # Errors
    ///
    /// Returns an error when auth is required and the header is missing, malformed, or invalid.
    pub fn validate_authorization_header(
        &self,
        authorization: Option<&str>,
    ) -> Result<(), HttpAuthError> {
        let Some(expected_token) = self.expected_token.as_deref() else {
            return Ok(());
        };
        let Some(header) = authorization
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Err(HttpAuthError::MissingAuthorization);
        };
        let Some((scheme, token)) = header.split_once(' ') else {
            return Err(HttpAuthError::InvalidAuthorizationScheme);
        };
        if !scheme.eq_ignore_ascii_case("Bearer") {
            return Err(HttpAuthError::InvalidAuthorizationScheme);
        }
        if token.trim() != expected_token {
            return Err(HttpAuthError::InvalidToken);
        }
        Ok(())
    }
}

/// Authentication errors returned by the HTTP adapter boundary.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpAuthError {
    /// Token auth is enabled but the configured token source did not produce a token.
    #[error("http auth token is missing from {token_env}")]
    MissingToken { token_env: String },
    /// The request did not include an Authorization header.
    #[error("http authorization header is required")]
    MissingAuthorization,
    /// The Authorization header did not use the Bearer scheme.
    #[error("http authorization header must use bearer token auth")]
    InvalidAuthorizationScheme,
    /// The bearer token did not match the configured token.
    #[error("http bearer token is invalid")]
    InvalidToken,
}

/// Errors returned by the localhost HTTP listener boundary.
#[derive(Debug, ThisError)]
pub enum HttpListenerError {
    /// Server configuration is unsafe or incomplete.
    #[error("http listener config is invalid: {message}")]
    Config { message: String },
    /// Bearer token configuration is unsafe or incomplete.
    #[error("http listener auth is invalid: {message}")]
    Auth { message: String },
    /// TCP listener or socket I/O failed.
    #[error("http listener io failed: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
    /// Incoming HTTP request could not be parsed.
    #[error("http request is invalid: {message}")]
    Request { message: String },
    /// Response serialization failed.
    #[error("http response serialization failed: {message}")]
    Response { message: String },
}

/// Minimal localhost HTTP adapter.
///
/// This listener owns only HTTP framing, bearer auth and registry routing. Runtime agent
/// execution still belongs to the injected `HttpRunDriver`.
pub struct HttpLocalServer {
    listener: TcpListener,
    validator: HttpAuthValidator,
    registry: Arc<HttpSessionRunRegistry>,
}

impl HttpLocalServer {
    /// Binds a local HTTP listener using an already resolved bearer token value.
    ///
    /// # Errors
    ///
    /// Returns an error when the config fails safety validation, required auth has no token, or
    /// the TCP listener cannot bind.
    pub async fn bind(
        config: HttpServerConfig,
        token: Option<&str>,
        registry: Arc<HttpSessionRunRegistry>,
    ) -> Result<Self, HttpListenerError> {
        config
            .validate()
            .map_err(|error| HttpListenerError::Config {
                message: error.to_string(),
            })?;
        let validator =
            config
                .auth
                .validator_from_token(token)
                .map_err(|error| HttpListenerError::Auth {
                    message: error.to_string(),
                })?;
        let listener = TcpListener::bind(config.bind_addr()).await?;
        Ok(Self {
            listener,
            validator,
            registry,
        })
    }

    /// Returns the actual bound address.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating system cannot report the bound address.
    pub fn local_addr(&self) -> Result<SocketAddr, HttpListenerError> {
        Ok(self.listener.local_addr()?)
    }

    /// Serves connections until `shutdown` resolves.
    ///
    /// # Errors
    ///
    /// Returns an error when accepting a TCP connection fails.
    pub async fn serve_until_shutdown<F>(self, shutdown: F) -> Result<(), HttpListenerError>
    where
        F: Future<Output = ()> + Send,
    {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => return Ok(()),
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted?;
                    let validator = self.validator.clone();
                    let registry = Arc::clone(&self.registry);
                    tokio::spawn(async move {
                        let _ = handle_http_connection(stream, validator, registry).await;
                    });
                }
            }
        }
    }
}

async fn handle_http_connection(
    mut stream: TcpStream,
    validator: HttpAuthValidator,
    registry: Arc<HttpSessionRunRegistry>,
) -> Result<(), HttpListenerError> {
    let response = match read_http_request(&mut stream).await {
        Ok(request) => route_http_request(request, &validator, &registry),
        Err(error) => http_error_response(400, "bad_request", error.to_string()),
    };
    write_http_response(&mut stream, response).await
}

fn route_http_request(
    request: HttpRequest,
    validator: &HttpAuthValidator,
    registry: &HttpSessionRunRegistry,
) -> HttpResponse {
    if request.method == "GET" && request.path == "/health" {
        return json_response(200, json!({ "status": "ok" }));
    }
    if request.method != "POST" {
        return http_error_response(404, "not_found", "http route not found");
    }
    if let Err(error) =
        validator.validate_authorization_header(request.header("authorization").map(String::as_str))
    {
        return http_error_response(401, "unauthorized", error.to_string());
    }

    if request.path == "/sessions" {
        let Ok(body) = parse_json_body::<HttpSessionCreateRequest>(&request.body) else {
            return http_error_response(400, "bad_request", "invalid session create body");
        };
        let session = registry.create_session(body);
        return json_response(201, json!(session));
    }

    if let Some(session_id) = request
        .path
        .strip_prefix("/sessions/")
        .and_then(|suffix| suffix.strip_suffix("/runs"))
        .filter(|session_id| !session_id.is_empty() && !session_id.contains('/'))
    {
        let Ok(command) =
            parse_json_body::<HttpCommandEnvelope<HttpRunStartRequest>>(&request.body)
        else {
            return http_error_response(400, "bad_request", "invalid run start command body");
        };
        return match registry.start_run_command(session_id, command) {
            Ok(receipt) => json_response(201, json!(receipt)),
            Err(error) => registry_error_response(error),
        };
    }

    if let Some((run_id, call_id)) = approval_route_parts(&request.path) {
        let Ok(command) =
            parse_json_body::<HttpCommandEnvelope<HttpApprovalDecisionRequest>>(&request.body)
        else {
            return http_error_response(400, "bad_request", "invalid approval command body");
        };
        return match registry.submit_approval_command(run_id, call_id, command) {
            Ok(receipt) => json_response(200, json!(receipt)),
            Err(error) => registry_error_response(error),
        };
    }

    http_error_response(404, "not_found", "http route not found")
}

fn approval_route_parts(path: &str) -> Option<(&str, &str)> {
    let suffix = path.strip_prefix("/runs/")?;
    let (run_id, call_id) = suffix.split_once("/approvals/")?;
    if run_id.is_empty() || run_id.contains('/') || call_id.is_empty() || call_id.contains('/') {
        return None;
    }
    Some((run_id, call_id))
}

fn parse_json_body<T: DeserializeOwned>(body: &[u8]) -> Result<T, serde_json::Error> {
    serde_json::from_slice(body)
}

async fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, HttpListenerError> {
    let mut buffer = Vec::new();
    let header_end = loop {
        if let Some(index) = find_header_end(&buffer) {
            break index;
        }
        if buffer.len() > HTTP_MAX_HEADER_BYTES {
            return Err(HttpListenerError::Request {
                message: "request headers exceed limit".to_owned(),
            });
        }
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(HttpListenerError::Request {
                message: "request closed before headers completed".to_owned(),
            });
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let header_bytes = &buffer[..header_end];
    let mut body = buffer[header_end + 4..].to_vec();
    let header_text = str::from_utf8(header_bytes).map_err(|error| HttpListenerError::Request {
        message: format!("request headers are not utf-8: {error}"),
    })?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().ok_or_else(|| HttpListenerError::Request {
        message: "missing request line".to_owned(),
    })?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| HttpListenerError::Request {
            message: "missing request method".to_owned(),
        })?
        .to_owned();
    let path = request_parts
        .next()
        .ok_or_else(|| HttpListenerError::Request {
            message: "missing request path".to_owned(),
        })?
        .to_owned();
    let mut headers = BTreeMap::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(HttpListenerError::Request {
                message: "malformed request header".to_owned(),
            });
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    let content_length = headers
        .get("content-length")
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|error| HttpListenerError::Request {
                    message: format!("invalid content-length: {error}"),
                })
        })
        .transpose()?
        .unwrap_or(0);
    if content_length > HTTP_MAX_BODY_BYTES {
        return Err(HttpListenerError::Request {
            message: "request body exceeds limit".to_owned(),
        });
    }
    while body.len() < content_length {
        let remaining = content_length - body.len();
        let mut chunk = vec![0_u8; remaining.min(4096)];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(HttpListenerError::Request {
                message: "request closed before body completed".to_owned(),
            });
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn write_http_response(
    stream: &mut TcpStream,
    response: HttpResponse,
) -> Result<(), HttpListenerError> {
    let body = serde_json::to_vec(&response.body).map_err(|error| HttpListenerError::Response {
        message: error.to_string(),
    })?;
    let head = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        response.status,
        http_reason(response.status),
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.shutdown().await?;
    Ok(())
}

fn registry_error_response(error: HttpRegistryError) -> HttpResponse {
    let status = match error {
        HttpRegistryError::SessionNotFound { .. } | HttpRegistryError::RunNotFound { .. } => 404,
        HttpRegistryError::UnsupportedProtocolVersion { .. }
        | HttpRegistryError::CommandSessionMismatch { .. }
        | HttpRegistryError::CommandPathSessionMismatch { .. }
        | HttpRegistryError::StaleCommandSequence { .. }
        | HttpRegistryError::RunNotActive { .. }
        | HttpRegistryError::ApprovalNotPending { .. }
        | HttpRegistryError::ApprovalModeDoesNotAsk { .. }
        | HttpRegistryError::ApprovalRequestChanged { .. }
        | HttpRegistryError::ApprovalToolCallChanged { .. }
        | HttpRegistryError::ApprovalPolicyChanged { .. }
        | HttpRegistryError::ApprovalExpiryChanged { .. }
        | HttpRegistryError::ApprovalExpired { .. } => 409,
        HttpRegistryError::EmptyPrompt | HttpRegistryError::MissingApprovalMode => 400,
        HttpRegistryError::DriverRejected { .. } => 500,
    };
    http_error_response(status, "registry_error", error.to_string())
}

fn json_response(status: u16, body: Value) -> HttpResponse {
    HttpResponse { status, body }
}

fn http_error_response(
    status: u16,
    code: impl Into<String>,
    message: impl Into<String>,
) -> HttpResponse {
    json_response(
        status,
        json!({
            "error": {
                "code": code.into(),
                "message": message.into()
            }
        }),
    )
}

fn http_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

struct HttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&String> {
        self.headers.get(name)
    }
}

struct HttpResponse {
    status: u16,
    body: Value,
}

/// One Server-Sent Events frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpSseEvent {
    id: Option<String>,
    event: String,
    data: String,
}

impl HttpSseEvent {
    /// Creates one SSE frame payload.
    ///
    /// # Errors
    ///
    /// Returns an error when the event name is empty or contains line breaks.
    pub fn new(event: impl Into<String>, data: impl Into<String>) -> Result<Self, HttpSseError> {
        Self::with_id(None, event, data)
    }

    /// Creates one SSE frame payload with an optional `id:` cursor.
    ///
    /// # Errors
    ///
    /// Returns an error when the event name or id is empty or contains line breaks.
    pub fn with_id(
        id: Option<String>,
        event: impl Into<String>,
        data: impl Into<String>,
    ) -> Result<Self, HttpSseError> {
        let event = event.into();
        if event.is_empty() || event.contains('\r') || event.contains('\n') {
            return Err(HttpSseError::InvalidEventName { event });
        }
        if let Some(id) = id.as_deref()
            && (id.trim().is_empty() || id.contains('\r') || id.contains('\n'))
        {
            return Err(HttpSseError::InvalidEventId { id: id.to_owned() });
        }
        Ok(Self {
            id,
            event,
            data: data.into(),
        })
    }

    /// Returns the optional SSE event id.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Returns the SSE event name.
    #[must_use]
    pub fn event(&self) -> &str {
        &self.event
    }

    /// Returns the serialized SSE data payload.
    #[must_use]
    pub fn data(&self) -> &str {
        &self.data
    }

    /// Encodes the frame using SSE `event:` and `data:` fields.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut encoded = String::new();
        if let Some(id) = &self.id {
            append_sse_field(&mut encoded, "id", id);
        }
        append_sse_field(&mut encoded, "event", &self.event);
        append_sse_field(&mut encoded, "data", &self.data);
        encoded.push('\n');
        encoded
    }
}

/// Errors returned while serializing HTTP SSE frames.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpSseError {
    /// The SSE event name is invalid.
    #[error("http sse event name is invalid: {event}")]
    InvalidEventName { event: String },
    /// The SSE event id is invalid.
    #[error("http sse event id is invalid: {id}")]
    InvalidEventId { id: String },
    /// The public run event could not be serialized to JSON.
    #[error("http run event serialization failed: {message}")]
    Serialize { message: String },
    /// A durable protocol cursor could not be generated.
    #[error("http protocol cursor is invalid: {message}")]
    Cursor { message: String },
}

/// Public replay class for HTTP protocol events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpProtocolEventClass {
    /// Replayable event derived from a durable or recovery-relevant fact.
    Durable,
    /// Process-local progress event that is not replayed after reconnect.
    Transient,
}

/// HTTP-facing protocol event envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpProtocolEvent {
    /// Protocol envelope schema version.
    pub schema_version: u32,
    /// Whether clients can expect this event to replay after reconnect.
    pub event_class: HttpProtocolEventClass,
    /// SSE `id:` value for durable events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_id: Option<String>,
    /// Public run event payload.
    pub run_event: PublicRunEvent,
}

impl HttpProtocolEvent {
    /// Wraps one public run event in the HTTP protocol envelope.
    ///
    /// # Errors
    ///
    /// Returns an error when a durable cursor cannot be generated for the event.
    pub fn from_run_event(event: PublicRunEvent) -> Result<Self, HttpProtocolCursorError> {
        let event_class = protocol_event_class(&event.event);
        let replay_id = match event_class {
            HttpProtocolEventClass::Durable => {
                Some(HttpProtocolCursor::from_run_event(&event)?.encode())
            }
            HttpProtocolEventClass::Transient => None,
        };
        Ok(Self {
            schema_version: HTTP_PROTOCOL_EVENT_SCHEMA_VERSION,
            event_class,
            replay_id,
            run_event: event,
        })
    }

    /// Returns whether this protocol event is replayable after reconnect.
    #[must_use]
    pub fn is_durable(&self) -> bool {
        self.event_class == HttpProtocolEventClass::Durable
    }

    /// Returns a DTO view that separates durable replayable events from transient live events.
    #[must_use]
    pub fn view(&self) -> HttpProtocolEventView {
        match self.event_class {
            HttpProtocolEventClass::Durable => {
                HttpProtocolEventView::Durable(HttpDurableEventView {
                    schema_version: self.schema_version,
                    replay_id: self.replay_id.clone().unwrap_or_default(),
                    run_event: self.run_event.clone(),
                })
            }
            HttpProtocolEventClass::Transient => {
                HttpProtocolEventView::Transient(HttpTransientEventView {
                    schema_version: self.schema_version,
                    run_event: self.run_event.clone(),
                })
            }
        }
    }
}

/// Explicit durable/transient event view used by future protocol clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event_class")]
pub enum HttpProtocolEventView {
    Durable(HttpDurableEventView),
    Transient(HttpTransientEventView),
}

/// Replayable event view with a cursor suitable for SSE `Last-Event-ID`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpDurableEventView {
    pub schema_version: u32,
    pub replay_id: String,
    pub run_event: PublicRunEvent,
}

/// Process-local event view that is not replayable after reconnect.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpTransientEventView {
    pub schema_version: u32,
    pub run_event: PublicRunEvent,
}

/// Durable HTTP replay cursor carried in SSE `id:` and `Last-Event-ID`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpProtocolCursor {
    pub session_id: String,
    pub run_id: String,
    pub sequence: u64,
}

impl HttpProtocolCursor {
    /// Builds a cursor for one public run event.
    ///
    /// # Errors
    ///
    /// Returns an error when a component cannot be encoded safely in an SSE id.
    pub fn from_run_event(event: &PublicRunEvent) -> Result<Self, HttpProtocolCursorError> {
        validate_cursor_component("session_id", &event.session_id)?;
        validate_cursor_component("run_id", &event.run_id)?;
        if event.sequence == 0 {
            return Err(HttpProtocolCursorError::InvalidSequence { sequence: 0 });
        }
        Ok(Self {
            session_id: event.session_id.clone(),
            run_id: event.run_id.clone(),
            sequence: event.sequence,
        })
    }

    /// Encodes this cursor for SSE `id:` / `Last-Event-ID`.
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{HTTP_PROTOCOL_CURSOR_PREFIX}:{}:{}:{}",
            self.session_id, self.run_id, self.sequence
        )
    }

    /// Parses an SSE `Last-Event-ID` cursor.
    ///
    /// # Errors
    ///
    /// Returns an error when the cursor is malformed or uses another cursor version.
    pub fn parse(value: &str) -> Result<Self, HttpProtocolCursorError> {
        let parts = value.split(':').collect::<Vec<_>>();
        if parts.len() != 4 || parts[0] != HTTP_PROTOCOL_CURSOR_PREFIX {
            return Err(HttpProtocolCursorError::InvalidFormat {
                cursor: value.to_owned(),
            });
        }
        validate_cursor_component("session_id", parts[1])?;
        validate_cursor_component("run_id", parts[2])?;
        let sequence =
            parts[3]
                .parse::<u64>()
                .map_err(|_| HttpProtocolCursorError::InvalidFormat {
                    cursor: value.to_owned(),
                })?;
        if sequence == 0 {
            return Err(HttpProtocolCursorError::InvalidSequence { sequence });
        }
        Ok(Self {
            session_id: parts[1].to_owned(),
            run_id: parts[2].to_owned(),
            sequence,
        })
    }
}

/// Cursor parsing and encoding errors.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpProtocolCursorError {
    /// Cursor does not match the HTTP protocol cursor format.
    #[error("invalid cursor format: {cursor}")]
    InvalidFormat { cursor: String },
    /// Cursor component cannot be represented safely inside an SSE id.
    #[error("invalid cursor component {component}: {value}")]
    InvalidComponent {
        component: &'static str,
        value: String,
    },
    /// Cursor sequence must be positive.
    #[error("invalid cursor sequence: {sequence}")]
    InvalidSequence { sequence: u64 },
}

/// Errors returned while replaying durable HTTP protocol events.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpProtocolReplayError {
    /// The provided cursor could not be parsed.
    #[error("http protocol replay cursor is invalid: {message}")]
    InvalidCursor { message: String },
    /// The cursor belongs to another session/run stream.
    #[error("http protocol replay cursor scope mismatch")]
    CursorScopeMismatch,
    /// The cursor is newer than the buffered run stream.
    #[error("http protocol replay cursor is ahead of buffered events")]
    CursorAhead,
}

/// Errors returned while receiving a transient live event.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpLiveEventRecvError {
    /// The subscriber lagged behind the bounded channel and one or more live events were dropped.
    #[error("http live event subscriber lagged and dropped {dropped} events")]
    Lagged { dropped: u64 },
    /// The live event bus was closed.
    #[error("http live event stream is closed")]
    Closed,
}

/// In-memory protocol event buffer used by HTTP/SSE adapters.
///
/// The buffer stores both durable and transient views for current subscribers, but reconnect replay
/// only returns durable events whose sequence is newer than the provided `Last-Event-ID` cursor.
#[derive(Default)]
pub struct HttpProtocolEventBuffer {
    events: Mutex<Vec<HttpProtocolEvent>>,
}

impl HttpProtocolEventBuffer {
    /// Creates an empty protocol event buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one public run event and returns the stored protocol envelope.
    ///
    /// # Errors
    ///
    /// Returns an error when a durable cursor cannot be generated.
    pub fn push_run_event(
        &self,
        event: PublicRunEvent,
    ) -> Result<HttpProtocolEvent, HttpProtocolCursorError> {
        let event = HttpProtocolEvent::from_run_event(event)?;
        self.events
            .lock()
            .expect("http protocol event buffer lock should not be poisoned")
            .push(event.clone());
        Ok(event)
    }

    /// Replays durable events for one run after an optional `Last-Event-ID` cursor.
    ///
    /// Transient protocol events are intentionally filtered out. A cursor from another run fails
    /// closed so clients cannot accidentally stitch together unrelated event streams.
    ///
    /// # Errors
    ///
    /// Returns an error when the cursor is malformed, belongs to another stream, or is ahead of the
    /// buffered stream.
    pub fn replay_run_after(
        &self,
        session_id: &str,
        run_id: &str,
        last_event_id: Option<&str>,
    ) -> Result<Vec<HttpProtocolEvent>, HttpProtocolReplayError> {
        let cursor = match last_event_id {
            Some(value) => Some(HttpProtocolCursor::parse(value).map_err(|error| {
                HttpProtocolReplayError::InvalidCursor {
                    message: error.to_string(),
                }
            })?),
            None => None,
        };
        if let Some(cursor) = &cursor
            && (cursor.session_id != session_id || cursor.run_id != run_id)
        {
            return Err(HttpProtocolReplayError::CursorScopeMismatch);
        }
        let after_sequence = cursor.as_ref().map_or(0, |cursor| cursor.sequence);
        let events = self
            .events
            .lock()
            .expect("http protocol event buffer lock should not be poisoned");
        let latest_sequence = events
            .iter()
            .filter(|event| {
                event.run_event.session_id == session_id && event.run_event.run_id == run_id
            })
            .map(|event| event.run_event.sequence)
            .max()
            .unwrap_or(0);
        if after_sequence > latest_sequence {
            return Err(HttpProtocolReplayError::CursorAhead);
        }
        Ok(events
            .iter()
            .filter(|event| {
                event.is_durable()
                    && event.run_event.session_id == session_id
                    && event.run_event.run_id == run_id
                    && event.run_event.sequence > after_sequence
            })
            .cloned()
            .collect())
    }
}

/// Bounded live event bus for local clients.
///
/// The bus broadcasts both durable and transient protocol events to active subscribers. Durable
/// replay still comes from `HttpProtocolEventBuffer`; lagged transient delivery is reported as a
/// live-stream drop and never mutates durable replay semantics.
pub struct HttpLiveEventBus {
    buffer: HttpProtocolEventBuffer,
    sender: broadcast::Sender<HttpProtocolEvent>,
}

impl HttpLiveEventBus {
    /// Creates a live bus with bounded subscriber capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (sender, _) = broadcast::channel(capacity);
        Self {
            buffer: HttpProtocolEventBuffer::new(),
            sender,
        }
    }

    /// Subscribes to live protocol events from this point forward.
    #[must_use]
    pub fn subscribe(&self) -> HttpLiveEventSubscriber {
        HttpLiveEventSubscriber {
            receiver: self.sender.subscribe(),
        }
    }

    /// Records one run event and broadcasts it to active subscribers.
    ///
    /// # Errors
    ///
    /// Returns an error when a durable cursor cannot be generated for the event.
    pub fn publish_run_event(
        &self,
        event: PublicRunEvent,
    ) -> Result<HttpProtocolEvent, HttpProtocolCursorError> {
        let event = self.buffer.push_run_event(event)?;
        let _ = self.sender.send(event.clone());
        Ok(event)
    }

    /// Replays durable events for one run after an optional cursor.
    ///
    /// # Errors
    ///
    /// Returns an error when the cursor is invalid, wrong-scope, or ahead of the buffer.
    pub fn replay_run_after(
        &self,
        session_id: &str,
        run_id: &str,
        last_event_id: Option<&str>,
    ) -> Result<Vec<HttpProtocolEvent>, HttpProtocolReplayError> {
        self.buffer
            .replay_run_after(session_id, run_id, last_event_id)
    }
}

/// Subscriber for bounded local live events.
pub struct HttpLiveEventSubscriber {
    receiver: broadcast::Receiver<HttpProtocolEvent>,
}

impl HttpLiveEventSubscriber {
    /// Receives one live protocol event.
    ///
    /// # Errors
    ///
    /// Returns `Lagged` when bounded live capacity dropped events, or `Closed` when the bus closes.
    pub async fn recv(&mut self) -> Result<HttpProtocolEvent, HttpLiveEventRecvError> {
        self.receiver.recv().await.map_err(|error| match error {
            broadcast::error::RecvError::Closed => HttpLiveEventRecvError::Closed,
            broadcast::error::RecvError::Lagged(dropped) => {
                HttpLiveEventRecvError::Lagged { dropped }
            }
        })
    }
}

/// Serializes one public run event into an SSE frame.
///
/// # Errors
///
/// Returns an error when the public event cannot be serialized.
pub fn public_run_event_to_sse(event: &PublicRunEvent) -> Result<HttpSseEvent, HttpSseError> {
    let protocol_event =
        HttpProtocolEvent::from_run_event(event.clone()).map_err(|error| HttpSseError::Cursor {
            message: error.to_string(),
        })?;
    let data = serde_json::to_string(&protocol_event).map_err(|error| HttpSseError::Serialize {
        message: error.to_string(),
    })?;
    HttpSseEvent::with_id(protocol_event.replay_id, HTTP_RUN_EVENT_SSE_NAME, data)
}

/// Sequence generator for public run events emitted by the HTTP adapter.
#[derive(Default)]
pub struct HttpRunEventSequencer {
    state: Mutex<BTreeMap<HttpRunSequenceKey, u64>>,
}

impl HttpRunEventSequencer {
    /// Creates an empty sequencer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates the next public event for a session/run pair.
    pub fn next_public_event(
        &self,
        session_id: &str,
        run_id: &str,
        event: PublicRunEventKind,
    ) -> PublicRunEvent {
        let sequence = self.next_sequence(session_id, run_id);
        PublicRunEvent::new(session_id, run_id, sequence, event)
    }

    /// Creates the next SSE frame for a session/run pair.
    ///
    /// # Errors
    ///
    /// Returns an error when the public event cannot be serialized.
    pub fn next_sse_event(
        &self,
        session_id: &str,
        run_id: &str,
        event: PublicRunEventKind,
    ) -> Result<HttpSseEvent, HttpSseError> {
        let event = self.next_public_event(session_id, run_id, event);
        public_run_event_to_sse(&event)
    }

    fn next_sequence(&self, session_id: &str, run_id: &str) -> u64 {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let key = HttpRunSequenceKey {
            session_id: session_id.to_owned(),
            run_id: run_id.to_owned(),
        };
        let sequence = state.entry(key).or_insert(0);
        *sequence += 1;
        *sequence
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct HttpRunSequenceKey {
    session_id: String,
    run_id: String,
}

fn append_sse_field(buffer: &mut String, field: &str, value: &str) {
    for line in value.split('\n') {
        buffer.push_str(field);
        buffer.push_str(": ");
        buffer.push_str(line);
        buffer.push('\n');
    }
}

fn protocol_event_class(event: &PublicRunEventKind) -> HttpProtocolEventClass {
    match event {
        PublicRunEventKind::TextDelta { .. }
        | PublicRunEventKind::ReasoningDelta { .. }
        | PublicRunEventKind::ToolCallArgsDelta { .. } => HttpProtocolEventClass::Transient,
        PublicRunEventKind::RunStarted { .. }
        | PublicRunEventKind::TaskRunStarted { .. }
        | PublicRunEventKind::RunFinished { .. }
        | PublicRunEventKind::TaskRunFinished { .. }
        | PublicRunEventKind::RunFailed { .. }
        | PublicRunEventKind::RunCancelled
        | PublicRunEventKind::ToolCallStarted { .. }
        | PublicRunEventKind::ToolCallCompleted { .. }
        | PublicRunEventKind::ApprovalRequested { .. }
        | PublicRunEventKind::ApprovalResolved { .. }
        | PublicRunEventKind::ToolResult { .. }
        | PublicRunEventKind::Usage { .. }
        | PublicRunEventKind::ContinuationState { .. }
        | PublicRunEventKind::Control { .. }
        | PublicRunEventKind::AssistantMessage { .. }
        | PublicRunEventKind::Notice { .. } => HttpProtocolEventClass::Durable,
    }
}

fn validate_cursor_component(
    component: &'static str,
    value: &str,
) -> Result<(), HttpProtocolCursorError> {
    if value.trim().is_empty()
        || value.contains(':')
        || value.contains('\r')
        || value.contains('\n')
    {
        return Err(HttpProtocolCursorError::InvalidComponent {
            component,
            value: value.to_owned(),
        });
    }
    Ok(())
}

/// Request body for creating one HTTP adapter session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct HttpSessionCreateRequest {
    /// Optional user-facing label for clients that manage multiple sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Public snapshot returned by session create/get endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpSessionSnapshot {
    /// HTTP adapter session id.
    pub id: String,
    /// Optional user-facing label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Runs that were registered under this HTTP session.
    #[serde(default)]
    pub run_ids: Vec<String>,
}

/// Request body for starting one run inside an HTTP adapter session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct HttpRunStartRequest {
    /// User prompt for the run.
    pub prompt: String,
    /// Explicit HTTP approval policy for the run.
    ///
    /// The HTTP adapter intentionally exposes `allow_readonly` instead of a broad `allow`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_mode: Option<HttpRunApprovalMode>,
}

/// Approval policy accepted by the HTTP run start endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpRunApprovalMode {
    /// Deny tool calls that need approval.
    Deny,
    /// Allow read-only work while keeping mutating operations gated by policy.
    AllowReadonly,
    /// Require an explicit approval endpoint decision for gated tool calls.
    Ask,
}

impl HttpRunApprovalMode {
    /// Returns the stable wire label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::AllowReadonly => "allow_readonly",
            Self::Ask => "ask",
        }
    }
}

impl fmt::Display for HttpRunApprovalMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Public run lifecycle state owned by the HTTP adapter registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpRunStatus {
    /// The registry has accepted the run but the driver has not acknowledged it yet.
    Starting,
    /// The driver accepted the run.
    Running,
    /// The run is waiting for at least one approval decision.
    WaitingForApproval,
    /// Cancellation has been requested and routed to the driver.
    CancelRequested,
    /// The run has finished.
    Finished,
    /// The run failed or the driver rejected startup.
    Failed,
}

impl HttpRunStatus {
    /// Returns whether the status is terminal for routing purposes.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Finished | Self::Failed)
    }
}

/// Public snapshot returned by run start/get/cancel endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpRunSnapshot {
    /// HTTP adapter run id.
    pub id: String,
    /// Owning HTTP adapter session id.
    pub session_id: String,
    /// Current adapter-visible run status.
    pub status: HttpRunStatus,
    /// Explicit approval mode provided when the run started.
    pub approval_mode: HttpRunApprovalMode,
    /// Bounded prompt preview for adapter clients.
    pub prompt_preview: String,
    /// Pending approval call ids in deterministic order.
    #[serde(default)]
    pub pending_approval_call_ids: Vec<String>,
    /// Registry-owned state sequence for stale-client command guards.
    pub stream_sequence: u64,
}

/// Pending approval metadata registered by a running HTTP adapter driver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpPendingApproval {
    /// Tool call id awaiting a user decision.
    pub call_id: String,
    /// Tool name shown to clients.
    pub tool_name: String,
    /// Stable id for this approval request.
    pub approval_request_id: String,
    /// Hash of the exact tool call payload being approved.
    pub tool_call_hash: String,
    /// Policy version used to request approval.
    pub policy_version: String,
    /// Expiry timestamp in Unix milliseconds.
    pub expires_at_ms: u64,
}

/// HTTP approval decision payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpApprovalDecisionRequest {
    /// Approval request id echoed from the pending approval snapshot.
    pub approval_request_id: String,
    /// Tool call hash echoed from the pending approval snapshot.
    pub tool_call_hash: String,
    /// Policy version echoed from the pending approval snapshot.
    pub policy_version: String,
    /// Expiry timestamp echoed from the pending approval snapshot.
    pub expires_at_ms: u64,
    /// Explicit decision for the pending approval.
    pub decision: HttpApprovalDecision,
    /// Optional user-facing reason for audit and display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// User decision submitted for one pending approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpApprovalDecision {
    /// Allow the pending tool call.
    Approve,
    /// Deny the pending tool call.
    Deny,
}

impl HttpApprovalDecision {
    /// Maps the HTTP-facing decision to the kernel's persisted approval decision.
    #[must_use]
    pub fn to_user_decision(self) -> ToolApprovalUserDecision {
        match self {
            Self::Approve => ToolApprovalUserDecision::Approved,
            Self::Deny => ToolApprovalUserDecision::Denied,
        }
    }
}

/// Stored and routed approval decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpApprovalDecisionRecord {
    /// Owning run id.
    pub run_id: String,
    /// Tool call id that was resolved.
    pub call_id: String,
    /// Kernel-compatible user decision.
    pub decision: ToolApprovalUserDecision,
    /// Optional user-facing reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Receipt for an envelope-routed approval command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpApprovalCommandReceipt {
    /// Command id used for retry de-duplication.
    pub command_id: String,
    /// Client that submitted the command.
    pub client_id: String,
    /// Session id from the command envelope.
    pub session_id: String,
    /// Run id receiving the approval.
    pub run_id: String,
    /// Tool call id receiving the approval.
    pub call_id: String,
    /// Optional optimistic state guard supplied by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_stream_sequence: Option<u64>,
    /// Optional durable correlation id supplied by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Decision routed to the run driver.
    pub decision: HttpApprovalDecisionRecord,
    /// Whether this response was replayed from a prior command id.
    pub replayed: bool,
}

impl HttpApprovalCommandReceipt {
    fn replayed(mut self) -> Self {
        self.replayed = true;
        self
    }
}

/// Receipt for an envelope-routed run start command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpRunStartCommandReceipt {
    /// Command id used for retry de-duplication.
    pub command_id: String,
    /// Client that submitted the command.
    pub client_id: String,
    /// Session id from the command envelope.
    pub session_id: String,
    /// Optional durable correlation id supplied by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Run snapshot produced by the existing registry/driver path.
    pub run: HttpRunSnapshot,
    /// Whether this response was replayed from a prior command id.
    pub replayed: bool,
}

impl HttpRunStartCommandReceipt {
    fn replayed(mut self) -> Self {
        self.replayed = true;
        self
    }
}

/// Start context delivered to the HTTP run driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRunDriverStart {
    /// Session snapshot at the moment the run was registered.
    pub session: HttpSessionSnapshot,
    /// Run snapshot in `starting` state.
    pub run: HttpRunSnapshot,
    /// Full prompt body. The preview is carried separately on the run snapshot.
    pub prompt: String,
}

/// Cancel context delivered to the HTTP run driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRunDriverCancel {
    /// Owning session id.
    pub session_id: String,
    /// Run id being canceled.
    pub run_id: String,
}

/// Approval context delivered to the HTTP run driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRunDriverApproval {
    /// Owning session id.
    pub session_id: String,
    /// Run id receiving the decision.
    pub run_id: String,
    /// Tool call id receiving the decision.
    pub call_id: String,
    /// Decision record routed to the driver.
    pub decision: HttpApprovalDecisionRecord,
}

/// Driver interface used by the HTTP registry.
///
/// The registry owns IDs and routing state. The driver owns actual agent execution,
/// cancellation, and approval delivery so this crate does not duplicate the agent loop.
pub trait HttpRunDriver: Send + Sync {
    /// Starts execution for a registered run.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying runtime cannot accept the run.
    fn start_run(&self, start: HttpRunDriverStart) -> Result<(), HttpRunDriverError>;

    /// Requests cancellation for a registered run.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying runtime cannot route the cancellation.
    fn cancel_run(&self, cancel: HttpRunDriverCancel) -> Result<(), HttpRunDriverError>;

    /// Routes a user approval decision to a registered run.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying runtime cannot route the approval decision.
    fn submit_approval(&self, approval: HttpRunDriverApproval) -> Result<(), HttpRunDriverError>;
}

/// Error returned by an HTTP run driver.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
#[error("{message}")]
pub struct HttpRunDriverError {
    /// Driver-provided error message.
    pub message: String,
}

impl HttpRunDriverError {
    /// Creates a driver error with context.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Errors returned by the HTTP session/run registry.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpRegistryError {
    /// The requested HTTP session does not exist.
    #[error("http session not found: {session_id}")]
    SessionNotFound { session_id: String },
    /// The requested HTTP run does not exist.
    #[error("http run not found: {run_id}")]
    RunNotFound { run_id: String },
    /// The run prompt is empty after trimming whitespace.
    #[error("http run start prompt must not be empty")]
    EmptyPrompt,
    /// The run did not include an explicit HTTP approval mode.
    #[error("http run start requires an explicit approval mode")]
    MissingApprovalMode,
    /// The run cannot accept this operation in its current state.
    #[error("http run {run_id} is not active")]
    RunNotActive { run_id: String },
    /// The approval call id is not currently pending for the run.
    #[error("http approval not pending for run {run_id} call {call_id}")]
    ApprovalNotPending { run_id: String, call_id: String },
    /// The run's approval mode does not use the approval endpoint.
    #[error("http run {run_id} approval mode {approval_mode} does not use approval endpoint")]
    ApprovalModeDoesNotAsk {
        run_id: String,
        approval_mode: HttpRunApprovalMode,
    },
    /// The underlying run driver rejected the registry operation.
    #[error("http driver rejected {operation} for run {run_id}: {message}")]
    DriverRejected {
        operation: &'static str,
        run_id: String,
        message: String,
    },
    /// The command envelope version is not supported.
    #[error("http command protocol version rejected: {message}")]
    UnsupportedProtocolVersion { message: String },
    /// The command envelope points to a different session than the addressed run.
    #[error(
        "http command session {command_session_id} does not match run {run_id} session {run_session_id}"
    )]
    CommandSessionMismatch {
        command_session_id: String,
        run_id: String,
        run_session_id: String,
    },
    /// The command envelope points to a different session than the addressed URL.
    #[error(
        "http command session {command_session_id} does not match path session {path_session_id}"
    )]
    CommandPathSessionMismatch {
        command_session_id: String,
        path_session_id: String,
    },
    /// The command was based on an older run stream sequence.
    #[error(
        "http command for run {run_id} is stale: expected stream sequence {expected}, current is {actual}"
    )]
    StaleCommandSequence {
        run_id: String,
        expected: u64,
        actual: u64,
    },
    /// The approval request id no longer matches the pending request.
    #[error("http approval request changed for run {run_id} call {call_id}")]
    ApprovalRequestChanged { run_id: String, call_id: String },
    /// The approval tool call hash no longer matches the pending request.
    #[error("http approval tool call changed for run {run_id} call {call_id}")]
    ApprovalToolCallChanged { run_id: String, call_id: String },
    /// The approval policy version no longer matches the pending request.
    #[error("http approval policy changed for run {run_id} call {call_id}")]
    ApprovalPolicyChanged { run_id: String, call_id: String },
    /// The approval expiry no longer matches the pending request.
    #[error("http approval expiry changed for run {run_id} call {call_id}")]
    ApprovalExpiryChanged { run_id: String, call_id: String },
    /// The approval request expired before the user decision arrived.
    #[error("http approval expired for run {run_id} call {call_id}")]
    ApprovalExpired { run_id: String, call_id: String },
}

/// In-memory registry for HTTP adapter sessions, runs, cancellations, and approvals.
pub struct HttpSessionRunRegistry {
    state: Mutex<HttpRegistryState>,
    driver: Arc<dyn HttpRunDriver>,
}

impl HttpSessionRunRegistry {
    /// Creates a registry that delegates execution to `driver`.
    #[must_use]
    pub fn new(driver: Arc<dyn HttpRunDriver>) -> Self {
        Self {
            state: Mutex::new(HttpRegistryState::default()),
            driver,
        }
    }

    /// Creates one HTTP adapter session.
    pub fn create_session(&self, request: HttpSessionCreateRequest) -> HttpSessionSnapshot {
        let mut state = self.lock_state();
        let id = state.next_session_id();
        let session = HttpSessionState {
            id: id.clone(),
            label: request.label,
            run_ids: Vec::new(),
        };
        let snapshot = session.snapshot();
        state.sessions.insert(id, session);
        snapshot
    }

    /// Returns one HTTP adapter session snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when `session_id` is unknown.
    pub fn get_session(&self, session_id: &str) -> Result<HttpSessionSnapshot, HttpRegistryError> {
        let state = self.lock_state();
        state
            .sessions
            .get(session_id)
            .map(HttpSessionState::snapshot)
            .ok_or_else(|| HttpRegistryError::SessionNotFound {
                session_id: session_id.to_owned(),
            })
    }

    /// Starts one run inside an existing HTTP adapter session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is unknown, the prompt is empty, approval mode is missing,
    /// or the driver rejects the run.
    pub fn start_run(
        &self,
        session_id: &str,
        request: HttpRunStartRequest,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        if request.prompt.trim().is_empty() {
            return Err(HttpRegistryError::EmptyPrompt);
        }
        let approval_mode = request
            .approval_mode
            .ok_or(HttpRegistryError::MissingApprovalMode)?;
        let prompt = request.prompt;
        let (run_id, session_snapshot, run_snapshot) = {
            let mut state = self.lock_state();
            let run_id = state.next_run_id();
            let session = state.sessions.get_mut(session_id).ok_or_else(|| {
                HttpRegistryError::SessionNotFound {
                    session_id: session_id.to_owned(),
                }
            })?;
            let run = HttpRunState::new(
                run_id.clone(),
                session_id.to_owned(),
                approval_mode,
                prompt_preview(&prompt),
            );
            session.run_ids.push(run_id.clone());
            let session_snapshot = session.snapshot();
            let run_snapshot = run.snapshot();
            state.runs.insert(run_id.clone(), run);
            (run_id, session_snapshot, run_snapshot)
        };

        let start = HttpRunDriverStart {
            session: session_snapshot,
            run: run_snapshot,
            prompt,
        };
        if let Err(error) = self.driver.start_run(start) {
            let mut state = self.lock_state();
            if let Some(run) = state.runs.get_mut(&run_id) {
                run.status = HttpRunStatus::Failed;
            }
            return Err(HttpRegistryError::DriverRejected {
                operation: "start",
                run_id,
                message: error.message,
            });
        }

        let mut state = self.lock_state();
        let run = state
            .runs
            .get_mut(&run_id)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.clone(),
            })?;
        if run.status == HttpRunStatus::Starting {
            run.status = HttpRunStatus::Running;
        }
        Ok(run.snapshot())
    }

    /// Starts one run from a command envelope with retry de-duplication.
    ///
    /// # Errors
    ///
    /// Returns an error when the command version is unsupported, the command session does not
    /// match the path session, the session/run request is invalid, or the driver rejects startup.
    pub fn start_run_command(
        &self,
        session_id: &str,
        command: HttpCommandEnvelope<HttpRunStartRequest>,
    ) -> Result<HttpRunStartCommandReceipt, HttpRegistryError> {
        command.ensure_supported().map_err(|error| {
            HttpRegistryError::UnsupportedProtocolVersion {
                message: error.to_string(),
            }
        })?;
        if command.session_id != session_id {
            return Err(HttpRegistryError::CommandPathSessionMismatch {
                command_session_id: command.session_id,
                path_session_id: session_id.to_owned(),
            });
        }
        let key = HttpCommandKey {
            session_id: command.session_id.clone(),
            client_id: command.client_id.clone(),
            command_id: command.command_id.clone(),
        };
        {
            let state = self.lock_state();
            if let Some(receipt) = state.run_start_command_receipts.get(&key) {
                return Ok(receipt.clone().replayed());
            }
        }

        let run = self.start_run(session_id, command.payload.clone())?;
        let receipt = HttpRunStartCommandReceipt {
            command_id: command.command_id,
            client_id: command.client_id,
            session_id: command.session_id,
            correlation_id: command.correlation_id,
            run,
            replayed: false,
        };
        let mut state = self.lock_state();
        state
            .run_start_command_receipts
            .insert(key, receipt.clone());
        Ok(receipt)
    }

    /// Returns one HTTP adapter run snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when `run_id` is unknown.
    pub fn get_run(&self, run_id: &str) -> Result<HttpRunSnapshot, HttpRegistryError> {
        let state = self.lock_state();
        state
            .runs
            .get(run_id)
            .map(HttpRunState::snapshot)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.to_owned(),
            })
    }

    /// Requests cancellation for a running HTTP adapter run.
    ///
    /// # Errors
    ///
    /// Returns an error when the run is unknown, terminal, or the driver rejects cancellation.
    pub fn cancel_run(&self, run_id: &str) -> Result<HttpRunSnapshot, HttpRegistryError> {
        let cancel = {
            let mut state = self.lock_state();
            let run = state
                .runs
                .get_mut(run_id)
                .ok_or_else(|| HttpRegistryError::RunNotFound {
                    run_id: run_id.to_owned(),
                })?;
            if run.status.is_terminal() {
                return Err(HttpRegistryError::RunNotActive {
                    run_id: run_id.to_owned(),
                });
            }
            if run.status == HttpRunStatus::CancelRequested {
                return Ok(run.snapshot());
            }
            run.previous_status = Some(run.status);
            run.status = HttpRunStatus::CancelRequested;
            HttpRunDriverCancel {
                session_id: run.session_id.clone(),
                run_id: run.id.clone(),
            }
        };

        if let Err(error) = self.driver.cancel_run(cancel) {
            let mut state = self.lock_state();
            if let Some(run) = state.runs.get_mut(run_id) {
                run.restore_previous_status();
            }
            return Err(HttpRegistryError::DriverRejected {
                operation: "cancel",
                run_id: run_id.to_owned(),
                message: error.message,
            });
        }

        self.get_run(run_id)
    }

    /// Registers one pending approval for an active run.
    ///
    /// # Errors
    ///
    /// Returns an error when the run is unknown or cannot accept approval work.
    pub fn register_approval_request(
        &self,
        run_id: &str,
        approval: HttpPendingApproval,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        let mut state = self.lock_state();
        let run = state
            .runs
            .get_mut(run_id)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.to_owned(),
            })?;
        if let Some(error) = run.approval_route_error(run_id, true) {
            return Err(error);
        }
        run.pending_approvals
            .insert(approval.call_id.clone(), approval);
        run.status = HttpRunStatus::WaitingForApproval;
        run.advance_stream_sequence();
        Ok(run.snapshot())
    }

    /// Routes one envelope-protected user approval command to an active run.
    ///
    /// # Errors
    ///
    /// Returns an error when the command is stale, duplicated with an unsupported version, points
    /// to the wrong session, or fails normal approval routing checks.
    pub fn submit_approval_command(
        &self,
        run_id: &str,
        call_id: &str,
        command: HttpCommandEnvelope<HttpApprovalDecisionRequest>,
    ) -> Result<HttpApprovalCommandReceipt, HttpRegistryError> {
        command.ensure_supported().map_err(|error| {
            HttpRegistryError::UnsupportedProtocolVersion {
                message: error.to_string(),
            }
        })?;
        let key = HttpCommandKey {
            session_id: command.session_id.clone(),
            client_id: command.client_id.clone(),
            command_id: command.command_id.clone(),
        };
        {
            let state = self.lock_state();
            if let Some(receipt) = state.command_receipts.get(&key) {
                return Ok(receipt.clone().replayed());
            }
            let run = state
                .runs
                .get(run_id)
                .ok_or_else(|| HttpRegistryError::RunNotFound {
                    run_id: run_id.to_owned(),
                })?;
            if run.session_id != command.session_id {
                return Err(HttpRegistryError::CommandSessionMismatch {
                    command_session_id: command.session_id,
                    run_id: run_id.to_owned(),
                    run_session_id: run.session_id.clone(),
                });
            }
            if let Some(expected) = command.expected_stream_sequence
                && expected != run.stream_sequence
            {
                return Err(HttpRegistryError::StaleCommandSequence {
                    run_id: run_id.to_owned(),
                    expected,
                    actual: run.stream_sequence,
                });
            }
        }

        let record = self.submit_approval_decision(run_id, call_id, command.payload.clone())?;
        let receipt = HttpApprovalCommandReceipt {
            command_id: command.command_id,
            client_id: command.client_id,
            session_id: command.session_id,
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
            expected_stream_sequence: command.expected_stream_sequence,
            correlation_id: command.correlation_id,
            decision: record,
            replayed: false,
        };
        let mut state = self.lock_state();
        state.command_receipts.insert(key, receipt.clone());
        Ok(receipt)
    }

    /// Routes one user approval decision to an active run.
    ///
    /// # Errors
    ///
    /// Returns an error when the run or call is unknown, the run cannot accept approval work, or the
    /// driver rejects the decision.
    pub fn submit_approval_decision(
        &self,
        run_id: &str,
        call_id: &str,
        request: HttpApprovalDecisionRequest,
    ) -> Result<HttpApprovalDecisionRecord, HttpRegistryError> {
        let (session_id, record) = {
            let mut state = self.lock_state();
            let run = state
                .runs
                .get_mut(run_id)
                .ok_or_else(|| HttpRegistryError::RunNotFound {
                    run_id: run_id.to_owned(),
                })?;
            if let Some(error) = run.approval_route_error(run_id, false) {
                return Err(error);
            }
            let pending = run.pending_approvals.get(call_id).ok_or_else(|| {
                HttpRegistryError::ApprovalNotPending {
                    run_id: run_id.to_owned(),
                    call_id: call_id.to_owned(),
                }
            })?;
            validate_approval_guard(run_id, call_id, pending, &request, current_unix_time_ms())?;
            let pending = run.pending_approvals.remove(call_id).ok_or_else(|| {
                HttpRegistryError::ApprovalNotPending {
                    run_id: run_id.to_owned(),
                    call_id: call_id.to_owned(),
                }
            })?;
            run.in_flight_approvals.insert(call_id.to_owned(), pending);
            let record = HttpApprovalDecisionRecord {
                run_id: run_id.to_owned(),
                call_id: call_id.to_owned(),
                decision: request.decision.to_user_decision(),
                reason: request.reason,
            };
            (run.session_id.clone(), record)
        };

        let approval = HttpRunDriverApproval {
            session_id,
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
            decision: record.clone(),
        };
        if let Err(error) = self.driver.submit_approval(approval) {
            let mut state = self.lock_state();
            if let Some(run) = state.runs.get_mut(run_id) {
                run.restore_in_flight_approval(call_id);
            }
            return Err(HttpRegistryError::DriverRejected {
                operation: "approval",
                run_id: run_id.to_owned(),
                message: error.message,
            });
        }

        let mut state = self.lock_state();
        let run = state
            .runs
            .get_mut(run_id)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.to_owned(),
            })?;
        run.in_flight_approvals.remove(call_id);
        run.approval_decisions.push(record.clone());
        if run.pending_approvals.is_empty()
            && run.in_flight_approvals.is_empty()
            && run.status == HttpRunStatus::WaitingForApproval
        {
            run.status = HttpRunStatus::Running;
        }
        run.advance_stream_sequence();
        Ok(record)
    }

    fn lock_state(&self) -> MutexGuard<'_, HttpRegistryState> {
        self.state
            .lock()
            .expect("http registry state lock should not be poisoned")
    }
}

#[derive(Default)]
struct HttpRegistryState {
    sessions: BTreeMap<String, HttpSessionState>,
    runs: BTreeMap<String, HttpRunState>,
    run_start_command_receipts: BTreeMap<HttpCommandKey, HttpRunStartCommandReceipt>,
    command_receipts: BTreeMap<HttpCommandKey, HttpApprovalCommandReceipt>,
    next_session_number: u64,
    next_run_number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct HttpCommandKey {
    session_id: String,
    client_id: String,
    command_id: String,
}

impl HttpRegistryState {
    fn next_session_id(&mut self) -> String {
        self.next_session_number += 1;
        format!("http-session-{}", self.next_session_number)
    }

    fn next_run_id(&mut self) -> String {
        self.next_run_number += 1;
        format!("http-run-{}", self.next_run_number)
    }
}

struct HttpSessionState {
    id: String,
    label: Option<String>,
    run_ids: Vec<String>,
}

impl HttpSessionState {
    fn snapshot(&self) -> HttpSessionSnapshot {
        HttpSessionSnapshot {
            id: self.id.clone(),
            label: self.label.clone(),
            run_ids: self.run_ids.clone(),
        }
    }
}

struct HttpRunState {
    id: String,
    session_id: String,
    status: HttpRunStatus,
    previous_status: Option<HttpRunStatus>,
    approval_mode: HttpRunApprovalMode,
    prompt_preview: String,
    pending_approvals: BTreeMap<String, HttpPendingApproval>,
    in_flight_approvals: BTreeMap<String, HttpPendingApproval>,
    approval_decisions: Vec<HttpApprovalDecisionRecord>,
    stream_sequence: u64,
}

impl HttpRunState {
    fn new(
        id: String,
        session_id: String,
        approval_mode: HttpRunApprovalMode,
        prompt_preview: String,
    ) -> Self {
        Self {
            id,
            session_id,
            status: HttpRunStatus::Starting,
            previous_status: None,
            approval_mode,
            prompt_preview,
            pending_approvals: BTreeMap::new(),
            in_flight_approvals: BTreeMap::new(),
            approval_decisions: Vec::new(),
            stream_sequence: 0,
        }
    }

    fn snapshot(&self) -> HttpRunSnapshot {
        HttpRunSnapshot {
            id: self.id.clone(),
            session_id: self.session_id.clone(),
            status: self.status,
            approval_mode: self.approval_mode,
            prompt_preview: self.prompt_preview.clone(),
            pending_approval_call_ids: self.pending_approvals.keys().cloned().collect(),
            stream_sequence: self.stream_sequence,
        }
    }

    fn approval_route_error(
        &self,
        run_id: &str,
        allow_starting: bool,
    ) -> Option<HttpRegistryError> {
        let status_accepts_approval = matches!(
            (self.status, allow_starting),
            (HttpRunStatus::Starting, true)
                | (HttpRunStatus::Running, _)
                | (HttpRunStatus::WaitingForApproval, _)
        );
        if !status_accepts_approval {
            return Some(HttpRegistryError::RunNotActive {
                run_id: run_id.to_owned(),
            });
        }
        if self.approval_mode != HttpRunApprovalMode::Ask {
            return Some(HttpRegistryError::ApprovalModeDoesNotAsk {
                run_id: run_id.to_owned(),
                approval_mode: self.approval_mode,
            });
        }
        None
    }

    fn restore_previous_status(&mut self) {
        if let Some(previous) = self.previous_status.take() {
            self.status = previous;
        }
    }

    fn restore_in_flight_approval(&mut self, call_id: &str) {
        if let Some(approval) = self.in_flight_approvals.remove(call_id) {
            self.pending_approvals.insert(call_id.to_owned(), approval);
        }
    }

    fn advance_stream_sequence(&mut self) {
        self.stream_sequence = self.stream_sequence.saturating_add(1);
    }
}

fn prompt_preview(prompt: &str) -> String {
    const MAX_PROMPT_PREVIEW_CHARS: usize = 120;
    let mut preview = prompt
        .chars()
        .take(MAX_PROMPT_PREVIEW_CHARS)
        .collect::<String>();
    if prompt.chars().count() > MAX_PROMPT_PREVIEW_CHARS {
        preview.push_str("...");
    }
    preview
}

fn validate_approval_guard(
    run_id: &str,
    call_id: &str,
    pending: &HttpPendingApproval,
    request: &HttpApprovalDecisionRequest,
    now_ms: u64,
) -> Result<(), HttpRegistryError> {
    if pending.approval_request_id != request.approval_request_id {
        return Err(HttpRegistryError::ApprovalRequestChanged {
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
        });
    }
    if pending.tool_call_hash != request.tool_call_hash {
        return Err(HttpRegistryError::ApprovalToolCallChanged {
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
        });
    }
    if pending.policy_version != request.policy_version {
        return Err(HttpRegistryError::ApprovalPolicyChanged {
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
        });
    }
    if pending.expires_at_ms != request.expires_at_ms {
        return Err(HttpRegistryError::ApprovalExpiryChanged {
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
        });
    }
    if now_ms >= pending.expires_at_ms {
        return Err(HttpRegistryError::ApprovalExpired {
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
        });
    }
    Ok(())
}

fn current_unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(u128::from(u64::MAX)) as u64
        })
}

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
