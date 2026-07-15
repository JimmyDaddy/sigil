use std::{collections::BTreeMap, future::Future, net::SocketAddr, str, sync::Arc, time::Duration};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sigil_kernel::PublicRunEventKind;
use thiserror::Error as ThisError;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinSet,
};

use crate::{
    auth::HttpAuthValidator,
    config::HttpServerConfig,
    disclosure::HttpDurableEgressDisclosureJournal,
    dto::{
        HttpApprovalDecisionRequest, HttpRunCancelRequest, HttpRunStartRequest,
        HttpSessionCreateRequest,
    },
    protocol::HttpCommandEnvelope,
    registry::{HttpRegistryError, HttpSessionRunRegistry},
    sse::{
        HTTP_RUN_EVENT_SSE_NAME, HttpLiveEventBus, HttpLiveEventRecvError, HttpProtocolEvent,
        HttpSseEvent,
    },
};

const HTTP_MAX_HEADER_BYTES: usize = 64 * 1024;
const HTTP_MAX_BODY_BYTES: usize = 1024 * 1024;
const HTTP_SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const HTTP_GRACEFUL_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

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
    event_bus: Arc<HttpLiveEventBus>,
    disclosure_journal: Option<Arc<HttpDurableEgressDisclosureJournal>>,
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
        Self::bind_with_event_bus(
            config,
            token,
            registry,
            Arc::new(HttpLiveEventBus::new(128)),
        )
        .await
    }

    /// Binds a local HTTP listener with an externally owned event bus.
    ///
    /// # Errors
    ///
    /// Returns an error when the config fails safety validation, required auth has no token, or
    /// the TCP listener cannot bind.
    pub async fn bind_with_event_bus(
        config: HttpServerConfig,
        token: Option<&str>,
        registry: Arc<HttpSessionRunRegistry>,
        event_bus: Arc<HttpLiveEventBus>,
    ) -> Result<Self, HttpListenerError> {
        Self::bind_with_surfaces(config, token, registry, event_bus, None).await
    }

    /// Binds the production listener with durable run and disclosure replay surfaces.
    ///
    /// # Errors
    ///
    /// Returns an error when production replay is not durable or listener safety validation fails.
    pub async fn bind_production(
        config: HttpServerConfig,
        token: Option<&str>,
        registry: Arc<HttpSessionRunRegistry>,
        event_bus: Arc<HttpLiveEventBus>,
        disclosure_journal: Arc<HttpDurableEgressDisclosureJournal>,
    ) -> Result<Self, HttpListenerError> {
        if !event_bus.has_durable_journal() {
            return Err(HttpListenerError::Config {
                message: "production listener requires a durable protocol journal".to_owned(),
            });
        }
        Self::bind_with_surfaces(config, token, registry, event_bus, Some(disclosure_journal)).await
    }

    async fn bind_with_surfaces(
        config: HttpServerConfig,
        token: Option<&str>,
        registry: Arc<HttpSessionRunRegistry>,
        event_bus: Arc<HttpLiveEventBus>,
        disclosure_journal: Option<Arc<HttpDurableEgressDisclosureJournal>>,
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
            event_bus,
            disclosure_journal,
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
        let (connection_shutdown, _) = watch::channel(false);
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted?;
                    let validator = self.validator.clone();
                    let registry = Arc::clone(&self.registry);
                    let event_bus = Arc::clone(&self.event_bus);
                    let disclosure_journal = self.disclosure_journal.clone();
                    let connection_shutdown = connection_shutdown.subscribe();
                    connections.spawn(async move {
                        handle_http_connection(
                            stream,
                            validator,
                            registry,
                            event_bus,
                            disclosure_journal,
                            connection_shutdown,
                        )
                        .await
                    });
                }
                Some(_joined) = connections.join_next(), if !connections.is_empty() => {}
            }
        }
        drop(self.listener);
        self.registry.begin_shutdown();
        let registry = Arc::clone(&self.registry);
        let cancellation = tokio::task::spawn_blocking(move || {
            registry.cancel_active_runs("HTTP server graceful shutdown")
        })
        .await
        .map_err(|_| HttpListenerError::Response {
            message: "HTTP shutdown cancellation worker failed".to_owned(),
        })?;
        let registry = Arc::clone(&self.registry);
        let drained = tokio::task::spawn_blocking(move || {
            registry.wait_for_driver_idle(HTTP_GRACEFUL_DRAIN_TIMEOUT)
        })
        .await
        .map_err(|_| HttpListenerError::Response {
            message: "HTTP shutdown drain worker failed".to_owned(),
        })?;
        let _ = connection_shutdown.send(true);
        while connections.join_next().await.is_some() {}
        cancellation
            .and(drained)
            .map_err(|error| HttpListenerError::Response {
                message: format!("HTTP shutdown could not drain every active run: {error}"),
            })
    }
}

async fn handle_http_connection(
    mut stream: TcpStream,
    validator: HttpAuthValidator,
    registry: Arc<HttpSessionRunRegistry>,
    event_bus: Arc<HttpLiveEventBus>,
    disclosure_journal: Option<Arc<HttpDurableEgressDisclosureJournal>>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), HttpListenerError> {
    let request = tokio::select! {
        request = read_http_request(&mut stream) => request,
        _ = wait_for_shutdown(&mut shutdown) => {
            stream.shutdown().await?;
            return Ok(());
        }
    };
    let response = match request {
        Ok(request) => {
            if request.method == "GET" && run_events_route_id(&request.path).is_some() {
                return stream_run_events(
                    &mut stream,
                    request,
                    &validator,
                    &registry,
                    &event_bus,
                    &mut shutdown,
                )
                .await;
            }
            match tokio::task::spawn_blocking(move || {
                route_http_request(
                    request,
                    &validator,
                    &registry,
                    disclosure_journal.as_deref(),
                )
            })
            .await
            {
                Ok(response) => response,
                Err(_) => {
                    http_error_response(500, "route_error", "http request routing worker failed")
                }
            }
        }
        Err(error) => http_error_response(400, "bad_request", error.to_string()),
    };
    tokio::select! {
        result = write_http_response(&mut stream, response) => result,
        _ = wait_for_shutdown(&mut shutdown) => {
            stream.shutdown().await?;
            Ok(())
        }
    }
}

fn route_http_request(
    request: HttpRequest,
    validator: &HttpAuthValidator,
    registry: &HttpSessionRunRegistry,
    disclosure_journal: Option<&HttpDurableEgressDisclosureJournal>,
) -> HttpResponse {
    if request.method == "GET" && request.path == "/health" {
        return json_response(200, json!({ "status": "ok" }));
    }
    if let Err(error) =
        validator.validate_authorization_header(request.header("authorization").map(String::as_str))
    {
        return http_error_response(401, "unauthorized", error.to_string());
    }

    if request.method == "GET" && request.path == "/sessions" {
        return json_response(200, json!({ "sessions": registry.list_sessions() }));
    }

    if request.method == "GET" && request.path == "/openapi.json" {
        return json_response(200, crate::http_openapi_document());
    }

    if request.method == "GET" && request.path == "/disclosures" {
        let Some(journal) = disclosure_journal else {
            return http_error_response(
                503,
                "disclosure_unavailable",
                "durable disclosure replay is unavailable",
            );
        };
        return match journal.replay_after(request.header("last-event-id").map(String::as_str)) {
            Ok(records) => json_response(200, json!({ "disclosures": records })),
            Err(error) => http_error_response(409, "replay_error", error.to_string()),
        };
    }

    if request.method == "POST" && request.path == "/sessions" {
        let Ok(body) = parse_json_body::<HttpSessionCreateRequest>(&request.body) else {
            return http_error_response(400, "bad_request", "invalid session create body");
        };
        return match registry.create_session(body) {
            Ok(session) => json_response(201, json!(session)),
            Err(error) => registry_error_response(error),
        };
    }

    if request.method == "GET"
        && let Some(session_id) = request
            .path
            .strip_prefix("/sessions/")
            .filter(|session_id| !session_id.is_empty() && !session_id.contains('/'))
    {
        return match registry.get_session(session_id) {
            Ok(session) => json_response(200, json!(session)),
            Err(error) => registry_error_response(error),
        };
    }

    if request.method == "POST"
        && let Some(session_id) = request
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

    if request.method == "GET"
        && let Some(run_id) = request
            .path
            .strip_prefix("/runs/")
            .filter(|run_id| !run_id.is_empty() && !run_id.contains('/'))
    {
        return match registry.get_run(run_id) {
            Ok(run) => json_response(200, json!(run)),
            Err(error) => registry_error_response(error),
        };
    }

    if request.method == "POST"
        && let Some(run_id) = request
            .path
            .strip_prefix("/runs/")
            .and_then(|suffix| suffix.strip_suffix("/cancel"))
            .filter(|run_id| !run_id.is_empty() && !run_id.contains('/'))
    {
        let Ok(command) =
            parse_json_body::<HttpCommandEnvelope<HttpRunCancelRequest>>(&request.body)
        else {
            return http_error_response(400, "bad_request", "invalid run cancel command body");
        };
        return match registry.cancel_run_command(run_id, command) {
            Ok(receipt) => json_response(200, json!(receipt)),
            Err(error) => registry_error_response(error),
        };
    }

    if request.method == "POST"
        && let Some((run_id, call_id)) = approval_route_parts(&request.path)
    {
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

async fn stream_run_events(
    stream: &mut TcpStream,
    request: HttpRequest,
    validator: &HttpAuthValidator,
    registry: &HttpSessionRunRegistry,
    event_bus: &HttpLiveEventBus,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), HttpListenerError> {
    if let Err(error) =
        validator.validate_authorization_header(request.header("authorization").map(String::as_str))
    {
        return write_http_response(
            stream,
            http_error_response(401, "unauthorized", error.to_string()),
        )
        .await;
    }
    let Some(run_id) = run_events_route_id(&request.path) else {
        return write_http_response(
            stream,
            http_error_response(404, "not_found", "http route not found"),
        )
        .await;
    };
    let mut subscriber = event_bus.subscribe();
    let run = match registry.get_run(run_id) {
        Ok(run) => run,
        Err(error) => return write_http_response(stream, registry_error_response(error)).await,
    };
    let session = match registry.get_session(&run.session_id) {
        Ok(session) => session,
        Err(error) => return write_http_response(stream, registry_error_response(error)).await,
    };
    let events = match event_bus.replay_run_after(
        &session.durable_session_scope_id,
        run_id,
        request.header("last-event-id").map(String::as_str),
    ) {
        Ok(events) => events,
        Err(error) => {
            return write_http_response(
                stream,
                http_error_response(409, "replay_error", error.to_string()),
            )
            .await;
        }
    };
    write_sse_response_head(stream).await?;
    let mut last_sequence = 0;
    for event in events {
        last_sequence = last_sequence.max(event.run_event.sequence);
        write_protocol_event(stream, &event).await?;
        if protocol_event_is_terminal(&event) {
            stream.shutdown().await?;
            return Ok(());
        }
    }
    if run.status.is_terminal() {
        stream.shutdown().await?;
        return Ok(());
    }
    stream.flush().await?;

    let mut keepalive = tokio::time::interval(HTTP_SSE_KEEPALIVE_INTERVAL);
    keepalive.tick().await;
    loop {
        tokio::select! {
            _ = wait_for_shutdown(shutdown) => {
                stream.shutdown().await?;
                return Ok(());
            }
            _ = keepalive.tick() => {
                stream.write_all(b": keep-alive\n\n").await?;
                stream.flush().await?;
            }
            received = subscriber.recv() => {
                let event = match received {
                    Ok(event) => event,
                    Err(HttpLiveEventRecvError::Lagged { dropped }) => {
                        let frame = HttpSseEvent::new(
                            "stream_gap",
                            json!({ "dropped_live_events": dropped }).to_string(),
                        ).map_err(|error| HttpListenerError::Response {
                            message: error.to_string(),
                        })?;
                        stream.write_all(frame.encode().as_bytes()).await?;
                        stream.shutdown().await?;
                        return Ok(());
                    }
                    Err(HttpLiveEventRecvError::Closed) => {
                        stream.shutdown().await?;
                        return Ok(());
                    }
                };
                if event.run_event.session_id != session.durable_session_scope_id
                    || event.run_event.run_id != run_id
                    || event.run_event.sequence <= last_sequence
                {
                    continue;
                }
                last_sequence = event.run_event.sequence;
                write_protocol_event(stream, &event).await?;
                stream.flush().await?;
                if protocol_event_is_terminal(&event) {
                    stream.shutdown().await?;
                    return Ok(());
                }
            }
        }
    }
}

fn run_events_route_id(path: &str) -> Option<&str> {
    path.strip_prefix("/runs/")
        .and_then(|suffix| suffix.strip_suffix("/events"))
        .filter(|run_id| !run_id.is_empty() && !run_id.contains('/'))
}

async fn write_sse_response_head(stream: &mut TcpStream) -> Result<(), HttpListenerError> {
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n",
        )
        .await?;
    Ok(())
}

async fn write_protocol_event(
    stream: &mut TcpStream,
    event: &HttpProtocolEvent,
) -> Result<(), HttpListenerError> {
    let data = serde_json::to_string(event).map_err(|error| HttpListenerError::Response {
        message: error.to_string(),
    })?;
    let frame = HttpSseEvent::with_id(event.replay_id.clone(), HTTP_RUN_EVENT_SSE_NAME, data)
        .map_err(|error| HttpListenerError::Response {
            message: error.to_string(),
        })?;
    stream.write_all(frame.encode().as_bytes()).await?;
    Ok(())
}

fn protocol_event_is_terminal(event: &HttpProtocolEvent) -> bool {
    matches!(
        event.run_event.event,
        PublicRunEventKind::RunFinished { .. }
            | PublicRunEventKind::RunFailed { .. }
            | PublicRunEventKind::RunCancelled
    )
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }
    while shutdown.changed().await.is_ok() {
        if *shutdown.borrow() {
            return;
        }
    }
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
    let head = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        response.status,
        http_reason(response.status),
        response.content_type,
        response.body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&response.body).await?;
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
        | HttpRegistryError::ApprovalExpired { .. }
        | HttpRegistryError::SessionForegroundRunActive { .. }
        | HttpRegistryError::CommandKeyConflict { .. }
        | HttpRegistryError::RunTerminalConflict { .. } => 409,
        HttpRegistryError::EmptyPrompt | HttpRegistryError::MissingApprovalMode => 400,
        HttpRegistryError::DriverRejected { .. }
        | HttpRegistryError::DriverPanicked { .. }
        | HttpRegistryError::SessionBindingRejected { .. }
        | HttpRegistryError::InvalidSessionBinding { .. }
        | HttpRegistryError::CommandExecutionAborted
        | HttpRegistryError::CommandIdentityEncodingFailed
        | HttpRegistryError::CommandIdentityPersistenceFailed { .. } => 500,
        HttpRegistryError::CommandRegistrySaturated | HttpRegistryError::ServerShuttingDown => 503,
    };
    http_error_response(status, "registry_error", error.to_string())
}

fn json_response(status: u16, body: Value) -> HttpResponse {
    match serde_json::to_vec(&body) {
        Ok(body) => bytes_response(status, "application/json", body),
        Err(error) => {
            let fallback = json!({
                "error": {
                    "code": "response_error",
                    "message": error.to_string()
                }
            });
            let fallback = match serde_json::to_vec(&fallback) {
                Ok(fallback) => fallback,
                Err(_) => b"{\"error\":{\"code\":\"response_error\"}}".to_vec(),
            };
            bytes_response(500, "application/json", fallback)
        }
    }
}

fn bytes_response(status: u16, content_type: &'static str, body: Vec<u8>) -> HttpResponse {
    HttpResponse {
        status,
        content_type,
        body,
    }
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
        503 => "Service Unavailable",
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
    content_type: &'static str,
    body: Vec<u8>,
}
