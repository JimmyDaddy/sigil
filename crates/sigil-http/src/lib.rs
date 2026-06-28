use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};
use sigil_kernel::{PublicRunEvent, PublicRunEventKind, ToolApprovalUserDecision};
use thiserror::Error as ThisError;

/// Environment variable read by the HTTP adapter for its bearer token by default.
pub const DEFAULT_HTTP_TOKEN_ENV: &str = "SIGIL_HTTP_TOKEN";
/// SSE event name used for public run events.
pub const HTTP_RUN_EVENT_SSE_NAME: &str = "run_event";
/// Current schema version for HTTP protocol event envelopes.
pub const HTTP_PROTOCOL_EVENT_SCHEMA_VERSION: u32 = 1;

const HTTP_PROTOCOL_CURSOR_PREFIX: &str = "sigil-http-run-v1";

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
}

/// Pending approval metadata registered by a running HTTP adapter driver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpPendingApproval {
    /// Tool call id awaiting a user decision.
    pub call_id: String,
    /// Tool name shown to clients.
    pub tool_name: String,
}

/// HTTP approval decision payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpApprovalDecisionRequest {
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
        Ok(run.snapshot())
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
    next_session_number: u64,
    next_run_number: u64,
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

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
