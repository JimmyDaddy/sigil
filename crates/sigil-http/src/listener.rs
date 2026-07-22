use std::{collections::BTreeMap, future::Future, net::SocketAddr, str, sync::Arc, time::Duration};

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sigil_kernel::PublicRunEventKind;
use sigil_runtime::{
    LocalSessionCatalogState, LocalSessionMutationError, SessionCatalogProjectionEntry,
    SessionCatalogProjectionError, SessionCatalogProjectionPage, SessionCatalogProjectionQuery,
    SessionCatalogProjectionService,
    application_run::{
        DEFAULT_APPLICATION_TRANSCRIPT_PAGE_SIZE, MAX_APPLICATION_TRANSCRIPT_PAGE_SIZE,
    },
    conversation_display::{
        DEFAULT_CONVERSATION_DISPLAY_PAGE_SIZE, MAX_CONVERSATION_DISPLAY_PAGE_SIZE,
    },
};
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
        HttpApprovalDecisionRequest, HttpRunCancelRequest, HttpRunStartRequest, HttpServerInfo,
        HttpSessionCatalogBatchExecuteRequest, HttpSessionCatalogBatchPlanRequest,
        HttpSessionCreateRequest, HttpSessionDeleteRequest, HttpSessionInvalidSourceDeleteReceipt,
        HttpSessionInvalidSourceDeleteRequest, HttpSessionMutationReceipt, HttpSessionOpenRequest,
        HttpSessionQuarantineReceipt, HttpSessionQuarantineRequest, HttpSessionRenameRequest,
        HttpVerificationRerunRequest,
    },
    protocol::HttpCommandEnvelope,
    registry::{HttpRegistryError, HttpSessionRunRegistry},
    session_catalog_batch::{
        SessionCatalogBatchError, execute_session_catalog_batch, plan_session_catalog_batch,
    },
    sse::{
        HTTP_RUN_EVENT_SSE_NAME, HttpLiveEventBus, HttpLiveEventRecvError, HttpProtocolEvent,
        HttpSseEvent,
    },
    support::HttpSupportContext,
};

const HTTP_MAX_HEADER_BYTES: usize = 64 * 1024;
const HTTP_MAX_BODY_BYTES: usize = 1024 * 1024;
const HTTP_SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const HTTP_GRACEFUL_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_SESSION_ID_HEADER: &str = "x-sigil-session-id";
const HTTP_OWNER_REVISION_HEADER: &str = "x-sigil-owner-revision";
const HTTP_MAX_CONVERSATION_DISPLAY_CURSOR_BYTES: usize = 4 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct HttpSessionCatalogPage {
    workspace_id: String,
    generation: u64,
    reconciled_at_unix_ms: u64,
    degraded_source_count: usize,
    identity_conflict_count: usize,
    truncated_source_count: usize,
    entries: Vec<HttpSessionCatalogEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct HttpSessionCatalogEntry {
    workspace_id: String,
    session_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    source_state: LocalSessionCatalogState,
    source_bytes: u64,
    source_modified_at_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    user_message_count: u64,
    assistant_message_count: u64,
    tool_result_count: u64,
    control_entry_count: u64,
    pinned: bool,
    indexed_at_unix_ms: u64,
}

impl From<SessionCatalogProjectionPage> for HttpSessionCatalogPage {
    fn from(page: SessionCatalogProjectionPage) -> Self {
        Self {
            workspace_id: page.workspace_id,
            generation: page.generation,
            reconciled_at_unix_ms: page.reconciled_at_unix_ms,
            degraded_source_count: page.degraded_source_count,
            identity_conflict_count: page.identity_conflict_count,
            truncated_source_count: page.truncated_source_count,
            entries: page.entries.into_iter().map(Into::into).collect(),
            next_cursor: page.next_cursor,
        }
    }
}

impl From<SessionCatalogProjectionEntry> for HttpSessionCatalogEntry {
    fn from(entry: SessionCatalogProjectionEntry) -> Self {
        Self {
            workspace_id: entry.workspace_id,
            session_ref: entry.session_ref,
            session_id: entry.session_id,
            source_state: entry.source_state,
            source_bytes: entry.source_bytes,
            source_modified_at_unix_ms: entry.source_modified_at_unix_ms,
            provider_name: entry.provider_name,
            model_name: entry.model_name,
            title: entry.title,
            user_message_count: entry.user_message_count,
            assistant_message_count: entry.assistant_message_count,
            tool_result_count: entry.tool_result_count,
            control_entry_count: entry.control_entry_count,
            pinned: entry.pinned,
            indexed_at_unix_ms: entry.indexed_at_unix_ms,
        }
    }
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
    event_bus: Arc<HttpLiveEventBus>,
    disclosure_journal: Option<Arc<HttpDurableEgressDisclosureJournal>>,
    session_catalog: Option<Arc<SessionCatalogProjectionService>>,
    server_info: Option<HttpServerInfo>,
    support_context: Option<Arc<HttpSupportContext>>,
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
        Self::bind_with_surfaces(config, token, registry, event_bus, None, None, None).await
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
        session_catalog: Arc<SessionCatalogProjectionService>,
        workspace_id: impl Into<String>,
        shutdown_on_stdin_close: bool,
    ) -> Result<Self, HttpListenerError> {
        if !event_bus.has_durable_journal() {
            return Err(HttpListenerError::Config {
                message: "production listener requires a durable protocol journal".to_owned(),
            });
        }
        Self::bind_with_surfaces(
            config,
            token,
            registry,
            event_bus,
            Some(disclosure_journal),
            Some(session_catalog),
            Some((workspace_id.into(), shutdown_on_stdin_close)),
        )
        .await
    }

    async fn bind_with_surfaces(
        config: HttpServerConfig,
        token: Option<&str>,
        registry: Arc<HttpSessionRunRegistry>,
        event_bus: Arc<HttpLiveEventBus>,
        disclosure_journal: Option<Arc<HttpDurableEgressDisclosureJournal>>,
        session_catalog: Option<Arc<SessionCatalogProjectionService>>,
        server_info_context: Option<(String, bool)>,
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
        let server_info = server_info_context
            .map(|(workspace_id, shutdown_on_stdin_close)| {
                listener.local_addr().map(|bind_addr| {
                    HttpServerInfo::new(workspace_id, bind_addr, shutdown_on_stdin_close)
                })
            })
            .transpose()?;
        Ok(Self {
            listener,
            validator,
            registry,
            event_bus,
            disclosure_journal,
            session_catalog,
            server_info,
            support_context: None,
        })
    }

    /// Attaches process-private support projection inputs to an already bound server.
    #[must_use]
    pub fn with_support_context(mut self, support_context: HttpSupportContext) -> Self {
        self.support_context = Some(Arc::new(support_context));
        self
    }

    /// Returns the actual bound address.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating system cannot report the bound address.
    pub fn local_addr(&self) -> Result<SocketAddr, HttpListenerError> {
        Ok(self.listener.local_addr()?)
    }

    /// Returns immutable metadata published for a production desktop/app-server listener.
    #[must_use]
    pub fn server_info(&self) -> Option<&HttpServerInfo> {
        self.server_info.as_ref()
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
                    let session_catalog = self.session_catalog.clone();
                    let server_info = self.server_info.clone();
                    let support_context = self.support_context.clone();
                    let connection_shutdown = connection_shutdown.subscribe();
                    connections.spawn(async move {
                        handle_http_connection(
                            stream,
                            validator,
                            registry,
                            event_bus,
                            disclosure_journal,
                            session_catalog,
                            server_info,
                            support_context,
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
    session_catalog: Option<Arc<SessionCatalogProjectionService>>,
    server_info: Option<HttpServerInfo>,
    support_context: Option<Arc<HttpSupportContext>>,
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
                    session_catalog.as_deref(),
                    server_info.as_ref(),
                    support_context.as_deref(),
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
    session_catalog: Option<&SessionCatalogProjectionService>,
    server_info: Option<&HttpServerInfo>,
    support_context: Option<&HttpSupportContext>,
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

    if request.method == "GET" && request.path == "/server-info" {
        return server_info.map_or_else(
            || {
                http_error_response(
                    503,
                    "server_info_unavailable",
                    "desktop server metadata is unavailable",
                )
            },
            |server_info| json_response(200, json!(server_info)),
        );
    }

    if request.method == "GET" && request.path == "/support/doctor" {
        let Some(support_context) = support_context else {
            return http_error_response(
                503,
                "support_unavailable",
                "desktop support diagnostics are unavailable",
            );
        };
        return match support_context.doctor_report() {
            Ok(report) => json_response(200, json!(report)),
            Err(_) => http_error_response(
                503,
                "support_unavailable",
                "desktop support diagnostics could not be projected",
            ),
        };
    }

    if request.method == "POST" && request.path == "/support/bundle" {
        let Some(support_context) = support_context else {
            return http_error_response(
                503,
                "support_unavailable",
                "desktop support bundle export is unavailable",
            );
        };
        return match support_context.support_bundle() {
            Ok(bundle) => json_response(200, json!(bundle)),
            Err(_) => http_error_response(
                503,
                "support_unavailable",
                "desktop support bundle could not be projected",
            ),
        };
    }

    if request.method == "GET" && request.path == "/session-catalog" {
        let Some(session_catalog) = session_catalog else {
            return http_error_response(
                503,
                "session_catalog_unavailable",
                "durable historical session catalog is unavailable",
            );
        };
        let query = match parse_session_catalog_query(request.query.as_deref()) {
            Ok(query) => query,
            Err(message) => return http_error_response(400, "invalid_query", message),
        };
        return match session_catalog.reconcile_and_query(query) {
            Ok(page) => json_response(200, json!(HttpSessionCatalogPage::from(page))),
            Err(SessionCatalogProjectionError::InvalidQuery { message }) => {
                http_error_response(400, "invalid_query", message)
            }
            Err(SessionCatalogProjectionError::InvalidCursor { message }) => {
                http_error_response(400, "invalid_cursor", message)
            }
            Err(SessionCatalogProjectionError::StaleCursor { .. }) => http_error_response(
                409,
                "stale_cursor",
                "session catalog changed; restart pagination from the first page",
            ),
            Err(_) => http_error_response(
                503,
                "session_catalog_unavailable",
                "durable historical session catalog is unavailable",
            ),
        };
    }

    if request.method == "POST" && request.path == "/session-catalog/batch/plan" {
        let Some(session_catalog) = session_catalog else {
            return http_error_response(
                503,
                "session_catalog_unavailable",
                "durable historical session catalog is unavailable",
            );
        };
        let Ok(body) = parse_json_body::<HttpSessionCatalogBatchPlanRequest>(&request.body) else {
            return http_error_response(
                400,
                "invalid_session_batch_request",
                "invalid session catalog batch plan body",
            );
        };
        return match plan_session_catalog_batch(session_catalog, registry, &body) {
            Ok(plan) => json_response(200, json!(plan)),
            Err(error) => session_catalog_batch_error_response(error),
        };
    }

    if request.method == "POST" && request.path == "/session-catalog/batch/execute" {
        let Some(session_catalog) = session_catalog else {
            return http_error_response(
                503,
                "session_catalog_unavailable",
                "durable historical session catalog is unavailable",
            );
        };
        let Ok(body) = parse_json_body::<HttpSessionCatalogBatchExecuteRequest>(&request.body)
        else {
            return http_error_response(
                400,
                "invalid_session_batch_request",
                "invalid session catalog batch execute body",
            );
        };
        return match execute_session_catalog_batch(session_catalog, registry, &body) {
            Ok(receipt) => json_response(200, json!(receipt)),
            Err(error) => session_catalog_batch_error_response(error),
        };
    }

    if request.method == "POST" && request.path == "/session-catalog/rename" {
        let Some(session_catalog) = session_catalog else {
            return http_error_response(
                503,
                "session_catalog_unavailable",
                "durable historical session catalog is unavailable",
            );
        };
        let Ok(body) = parse_json_body::<HttpSessionRenameRequest>(&request.body) else {
            return http_error_response(
                400,
                "invalid_session_mutation_request",
                "invalid session rename body",
            );
        };
        let guard = match registry.reserve_durable_session_mutation(&body.session_id) {
            Ok(guard) => guard,
            Err(error) => return registry_error_response(error),
        };
        return match session_catalog.rename_session(
            &body.session_ref,
            &body.session_id,
            &body.display_name,
        ) {
            Ok(receipt) => {
                guard.finish(false);
                json_response(200, json!(HttpSessionMutationReceipt::from(receipt)))
            }
            Err(error) => session_mutation_error_response(error),
        };
    }

    if request.method == "POST" && request.path == "/session-catalog/delete" {
        let Some(session_catalog) = session_catalog else {
            return http_error_response(
                503,
                "session_catalog_unavailable",
                "durable historical session catalog is unavailable",
            );
        };
        let Ok(body) = parse_json_body::<HttpSessionDeleteRequest>(&request.body) else {
            return http_error_response(
                400,
                "invalid_session_mutation_request",
                "invalid session delete body",
            );
        };
        let guard = match registry.reserve_durable_session_mutation(&body.session_id) {
            Ok(guard) => guard,
            Err(error) => return registry_error_response(error),
        };
        return match session_catalog.delete_session(&body.session_ref, &body.session_id) {
            Ok(receipt) => {
                guard.finish(true);
                json_response(200, json!(HttpSessionMutationReceipt::from(receipt)))
            }
            Err(error) => session_mutation_error_response(error),
        };
    }

    if request.method == "POST" && request.path == "/session-catalog/quarantine" {
        let Some(session_catalog) = session_catalog else {
            return http_error_response(
                503,
                "session_catalog_unavailable",
                "durable historical session catalog is unavailable",
            );
        };
        let Ok(body) = parse_json_body::<HttpSessionQuarantineRequest>(&request.body) else {
            return http_error_response(
                400,
                "invalid_session_mutation_request",
                "invalid session quarantine body",
            );
        };
        return match session_catalog.quarantine_invalid_source(
            &body.session_ref,
            body.source_bytes,
            body.source_modified_at_unix_ms,
        ) {
            Ok(receipt) => json_response(200, json!(HttpSessionQuarantineReceipt::from(receipt))),
            Err(error) => session_mutation_error_response(error),
        };
    }

    if request.method == "POST" && request.path == "/session-catalog/delete-invalid-source" {
        let Some(session_catalog) = session_catalog else {
            return http_error_response(
                503,
                "session_catalog_unavailable",
                "durable historical session catalog is unavailable",
            );
        };
        let Ok(body) = parse_json_body::<HttpSessionInvalidSourceDeleteRequest>(&request.body)
        else {
            return http_error_response(
                400,
                "invalid_session_mutation_request",
                "invalid session source delete body",
            );
        };
        return match session_catalog.delete_invalid_source(
            &body.session_ref,
            body.source_bytes,
            body.source_modified_at_unix_ms,
        ) {
            Ok(receipt) => json_response(
                200,
                json!(HttpSessionInvalidSourceDeleteReceipt::from(receipt)),
            ),
            Err(error) => session_mutation_error_response(error),
        };
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

    if request.method == "POST" && request.path == "/sessions/open" {
        let Ok(body) = parse_json_body::<HttpSessionOpenRequest>(&request.body) else {
            return http_error_response(
                400,
                "invalid_session_open_request",
                "invalid session open body",
            );
        };
        return match registry.open_session(body) {
            Ok(session) => json_response(200, json!(session)),
            Err(error) => registry_error_response(error),
        };
    }

    if request.method == "GET"
        && let Some(session_id) = request
            .path
            .strip_prefix("/sessions/")
            .and_then(|suffix| suffix.strip_suffix("/continuity"))
            .filter(|session_id| !session_id.is_empty() && !session_id.contains('/'))
    {
        return match registry.session_continuity(session_id) {
            Ok(view) => json_response(200, json!(view)),
            Err(error) => registry_error_response(error),
        };
    }

    if request.method == "GET"
        && let Some(session_id) = request
            .path
            .strip_prefix("/sessions/")
            .and_then(|suffix| suffix.strip_suffix("/transcript"))
            .filter(|session_id| !session_id.is_empty() && !session_id.contains('/'))
    {
        let (before, limit) = match parse_transcript_query(request.query.as_deref()) {
            Ok(query) => query,
            Err(message) => return http_error_response(400, "invalid_query", message),
        };
        return match registry.transcript_page(session_id, before, limit) {
            Ok(page) => json_response(200, json!(page)),
            Err(error) => registry_error_response(error),
        };
    }

    if request.method == "GET"
        && let Some(session_id) = request
            .path
            .strip_prefix("/sessions/")
            .and_then(|suffix| suffix.strip_suffix("/display"))
            .filter(|session_id| !session_id.is_empty() && !session_id.contains('/'))
    {
        let (cursor, limit) = match parse_conversation_display_query(request.query.as_deref()) {
            Ok(query) => query,
            Err(HttpConversationDisplayQueryError::InvalidCursor(message)) => {
                return http_error_response(400, "invalid_display_cursor", message);
            }
            Err(HttpConversationDisplayQueryError::InvalidQuery(message)) => {
                return http_error_response(400, "invalid_query", message);
            }
        };
        return match registry.conversation_display_page(session_id, cursor.as_deref(), limit) {
            Ok(page) => json_response(200, json!(page)),
            Err(error) => registry_error_response(error),
        };
    }

    if request.method == "GET"
        && let Some(session_id) = request
            .path
            .strip_prefix("/sessions/")
            .and_then(|suffix| suffix.strip_suffix("/run-context"))
            .filter(|session_id| !session_id.is_empty() && !session_id.contains('/'))
    {
        return match registry.run_context_view(session_id) {
            Ok(view) => json_response(200, json!(view)),
            Err(error) => registry_error_response(error),
        };
    }

    if request.method == "GET"
        && let Some(session_id) = request
            .path
            .strip_prefix("/sessions/")
            .and_then(|suffix| suffix.strip_suffix("/verification"))
            .filter(|session_id| !session_id.is_empty() && !session_id.contains('/'))
    {
        return match registry.verification_view(session_id) {
            Ok(Some(view)) => json_response(200, json!(view)),
            Ok(None) => http_error_response(
                404,
                "verification_not_found",
                "session has no task verification projection",
            ),
            Err(error) => registry_error_response(error),
        };
    }

    if request.method == "GET"
        && let Some(session_id) = request
            .path
            .strip_prefix("/sessions/")
            .and_then(|suffix| suffix.strip_suffix("/agent-activity"))
            .filter(|session_id| !session_id.is_empty() && !session_id.contains('/'))
    {
        return match registry.agent_activity_view(session_id) {
            Ok(view) => json_response(200, json!(view)),
            Err(error) => registry_error_response(error),
        };
    }

    if request.method == "POST"
        && let Some(session_id) = request
            .path
            .strip_prefix("/sessions/")
            .and_then(|suffix| suffix.strip_suffix("/verification/rerun"))
            .filter(|session_id| !session_id.is_empty() && !session_id.contains('/'))
    {
        let Ok(command) =
            parse_json_body::<HttpCommandEnvelope<HttpVerificationRerunRequest>>(&request.body)
        else {
            return http_error_response(
                400,
                "bad_request",
                "invalid verification rerun command body",
            );
        };
        return match registry.rerun_verification_command(session_id, command) {
            Ok(receipt) => json_response(200, json!(receipt)),
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
    let Some(session_id) = request
        .header(HTTP_SESSION_ID_HEADER)
        .filter(|value| !value.is_empty() && value.len() <= 512)
    else {
        return write_http_response(
            stream,
            http_error_response(
                400,
                "run_owner_admission_required",
                "run event streams require an exact session and foreground owner revision",
            ),
        )
        .await;
    };
    let Some(owner_revision) = request
        .header(HTTP_OWNER_REVISION_HEADER)
        .filter(|value| valid_owner_revision(value))
    else {
        return write_http_response(
            stream,
            http_error_response(
                400,
                "run_owner_admission_required",
                "run event streams require an exact session and foreground owner revision",
            ),
        )
        .await;
    };
    let mut subscriber = event_bus.subscribe();
    let (session, run) = match registry.admit_run_event_stream(session_id, run_id, owner_revision) {
        Ok(admission) => admission,
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

fn valid_owner_revision(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
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

fn parse_session_catalog_query(
    raw_query: Option<&str>,
) -> Result<SessionCatalogProjectionQuery, String> {
    let Some(raw_query) = raw_query.filter(|query| !query.is_empty()) else {
        return Ok(SessionCatalogProjectionQuery::default());
    };
    validate_percent_encoding(raw_query)?;
    let mut query = SessionCatalogProjectionQuery::default();
    let mut seen = BTreeMap::new();
    for (name, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
        if name.contains('\u{fffd}') || value.contains('\u{fffd}') {
            return Err("query must use valid UTF-8".to_owned());
        }
        let name = name.into_owned();
        if seen.insert(name.clone(), ()).is_some() {
            return Err(format!("query parameter '{name}' must appear at most once"));
        }
        let value = value.into_owned();
        match name.as_str() {
            "limit" => {
                query.limit = value
                    .parse()
                    .map_err(|_| "limit must be a positive integer".to_owned())?;
            }
            "cursor" => query.cursor = Some(value),
            "q" => query.search = Some(value),
            "provider" => query.provider_name = Some(value),
            "pinned" => {
                query.pinned = Some(match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err("pinned must be 'true' or 'false'".to_owned()),
                });
            }
            "state" => {
                query.source_state = Some(match value.as_str() {
                    "ready" => LocalSessionCatalogState::Ready,
                    "oversized" => LocalSessionCatalogState::Oversized,
                    "scan_budget_exceeded" => LocalSessionCatalogState::ScanBudgetExceeded,
                    "unsupported_legacy" => LocalSessionCatalogState::UnsupportedLegacy,
                    "invalid" => LocalSessionCatalogState::Invalid,
                    _ => {
                        return Err(
                            "state must be ready, oversized, scan_budget_exceeded, unsupported_legacy, or invalid"
                                .to_owned(),
                        );
                    }
                });
            }
            _ => return Err(format!("unknown query parameter '{name}'")),
        }
    }
    Ok(query)
}

fn parse_transcript_query(raw_query: Option<&str>) -> Result<(Option<u64>, usize), String> {
    let Some(raw_query) = raw_query.filter(|query| !query.is_empty()) else {
        return Ok((None, DEFAULT_APPLICATION_TRANSCRIPT_PAGE_SIZE));
    };
    validate_percent_encoding(raw_query)?;
    let mut before = None;
    let mut limit = DEFAULT_APPLICATION_TRANSCRIPT_PAGE_SIZE;
    let mut seen = BTreeMap::new();
    for (name, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
        if name.contains('\u{fffd}') || value.contains('\u{fffd}') {
            return Err("query must use valid UTF-8".to_owned());
        }
        let name = name.into_owned();
        if seen.insert(name.clone(), ()).is_some() {
            return Err(format!("query parameter '{name}' must appear at most once"));
        }
        let value = value.into_owned();
        match name.as_str() {
            "limit" => {
                limit = value
                    .parse::<usize>()
                    .map_err(|_| "limit must be a positive integer".to_owned())?;
                if !(1..=MAX_APPLICATION_TRANSCRIPT_PAGE_SIZE).contains(&limit) {
                    return Err(format!(
                        "limit must be between 1 and {MAX_APPLICATION_TRANSCRIPT_PAGE_SIZE}"
                    ));
                }
            }
            "before" => {
                let ordinal = value
                    .parse::<u64>()
                    .map_err(|_| "before must be a positive integer".to_owned())?;
                if ordinal == 0 {
                    return Err("before must be a positive integer".to_owned());
                }
                before = Some(ordinal);
            }
            _ => return Err(format!("unsupported query parameter '{name}'")),
        }
    }
    Ok((before, limit))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HttpConversationDisplayQueryError {
    InvalidCursor(String),
    InvalidQuery(String),
}

fn parse_conversation_display_query(
    raw_query: Option<&str>,
) -> Result<(Option<String>, usize), HttpConversationDisplayQueryError> {
    let Some(raw_query) = raw_query.filter(|query| !query.is_empty()) else {
        return Ok((None, DEFAULT_CONVERSATION_DISPLAY_PAGE_SIZE));
    };
    validate_percent_encoding(raw_query)
        .map_err(HttpConversationDisplayQueryError::InvalidQuery)?;
    let mut cursor = None;
    let mut limit = DEFAULT_CONVERSATION_DISPLAY_PAGE_SIZE;
    let mut seen = BTreeMap::new();
    for (name, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
        if name.contains('\u{fffd}') || value.contains('\u{fffd}') {
            return Err(HttpConversationDisplayQueryError::InvalidQuery(
                "query must use valid UTF-8".to_owned(),
            ));
        }
        let name = name.into_owned();
        if seen.insert(name.clone(), ()).is_some() {
            let message = format!("query parameter '{name}' must appear at most once");
            return Err(if name == "cursor" {
                HttpConversationDisplayQueryError::InvalidCursor(message)
            } else {
                HttpConversationDisplayQueryError::InvalidQuery(message)
            });
        }
        let value = value.into_owned();
        match name.as_str() {
            "limit" => {
                limit = value.parse::<usize>().map_err(|_| {
                    HttpConversationDisplayQueryError::InvalidQuery(
                        "limit must be a positive integer".to_owned(),
                    )
                })?;
                if !(1..=MAX_CONVERSATION_DISPLAY_PAGE_SIZE).contains(&limit) {
                    return Err(HttpConversationDisplayQueryError::InvalidQuery(format!(
                        "limit must be between 1 and {MAX_CONVERSATION_DISPLAY_PAGE_SIZE}"
                    )));
                }
            }
            "cursor" => {
                validate_conversation_display_cursor(&value)?;
                cursor = Some(value);
            }
            _ => {
                return Err(HttpConversationDisplayQueryError::InvalidQuery(format!(
                    "unsupported query parameter '{name}'"
                )));
            }
        }
    }
    Ok((cursor, limit))
}

fn validate_conversation_display_cursor(
    cursor: &str,
) -> Result<(), HttpConversationDisplayQueryError> {
    if cursor.is_empty() || cursor.len() > HTTP_MAX_CONVERSATION_DISPLAY_CURSOR_BYTES {
        return Err(HttpConversationDisplayQueryError::InvalidCursor(
            "display cursor has invalid size".to_owned(),
        ));
    }
    if !cursor
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(HttpConversationDisplayQueryError::InvalidCursor(
            "display cursor must be unpadded base64url".to_owned(),
        ));
    }
    Ok(())
}

fn validate_percent_encoding(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return Err("query contains invalid percent encoding".to_owned());
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    Ok(())
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
    let request_target = request_parts
        .next()
        .ok_or_else(|| HttpListenerError::Request {
            message: "missing request path".to_owned(),
        })?
        .to_owned();
    if request_target.contains('#') {
        return Err(HttpListenerError::Request {
            message: "request target must not contain a fragment".to_owned(),
        });
    }
    let (path, query) = request_target
        .split_once('?')
        .map_or((request_target.as_str(), None), |(path, query)| {
            (path, Some(query.to_owned()))
        });
    if !path.starts_with('/') {
        return Err(HttpListenerError::Request {
            message: "request path must be absolute".to_owned(),
        });
    }
    let path = path.to_owned();
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
        query,
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
    let status = match &error {
        HttpRegistryError::SessionNotFound { .. } | HttpRegistryError::RunNotFound { .. } => 404,
        HttpRegistryError::DurableSessionNotFound => 404,
        HttpRegistryError::UnsupportedProtocolVersion { .. }
        | HttpRegistryError::CommandSessionMismatch { .. }
        | HttpRegistryError::CommandPathSessionMismatch { .. }
        | HttpRegistryError::StaleCommandSequence { .. }
        | HttpRegistryError::RunNotActive { .. }
        | HttpRegistryError::RunNoLongerForeground { .. }
        | HttpRegistryError::RunOwnerChanged { .. }
        | HttpRegistryError::ApprovalNotPending { .. }
        | HttpRegistryError::ApprovalRequestChanged { .. }
        | HttpRegistryError::ApprovalToolCallChanged { .. }
        | HttpRegistryError::ApprovalPolicyChanged { .. }
        | HttpRegistryError::ApprovalExpiryChanged { .. }
        | HttpRegistryError::ApprovalDecisionUnavailable { .. }
        | HttpRegistryError::ApprovalExpired { .. }
        | HttpRegistryError::SessionForegroundRunActive { .. }
        | HttpRegistryError::SessionVerificationActive { .. }
        | HttpRegistryError::DurableSessionMutationActive
        | HttpRegistryError::CommandKeyConflict { .. }
        | HttpRegistryError::RunTerminalConflict { .. }
        | HttpRegistryError::DurableSessionNotReady
        | HttpRegistryError::DurableSessionIdentityChanged
        | HttpRegistryError::ConversationDisplayCursorStale => 409,
        HttpRegistryError::EmptyPrompt
        | HttpRegistryError::MissingPermissionMode
        | HttpRegistryError::InvalidSessionOpenRequest
        | HttpRegistryError::ConversationDisplayCursorInvalid => 400,
        HttpRegistryError::DriverRejected { .. }
        | HttpRegistryError::DriverPanicked { .. }
        | HttpRegistryError::SessionBindingRejected { .. }
        | HttpRegistryError::InvalidSessionBinding { .. }
        | HttpRegistryError::CommandExecutionAborted
        | HttpRegistryError::CommandIdentityEncodingFailed
        | HttpRegistryError::CommandIdentityPersistenceFailed { .. } => 500,
        HttpRegistryError::CommandRegistrySaturated
        | HttpRegistryError::ServerShuttingDown
        | HttpRegistryError::DurableSessionUnavailable
        | HttpRegistryError::ConversationDisplayUnavailable => 503,
    };
    let code = match &error {
        HttpRegistryError::InvalidSessionOpenRequest => "invalid_session_open_request",
        HttpRegistryError::DurableSessionNotFound => "durable_session_not_found",
        HttpRegistryError::DurableSessionNotReady => "durable_session_not_ready",
        HttpRegistryError::DurableSessionIdentityChanged => "durable_session_identity_changed",
        HttpRegistryError::DurableSessionUnavailable => "durable_session_unavailable",
        HttpRegistryError::RunNoLongerForeground { .. } => "run_no_longer_foreground",
        HttpRegistryError::RunOwnerChanged { .. } => "run_owner_changed",
        HttpRegistryError::ConversationDisplayCursorInvalid => "invalid_display_cursor",
        HttpRegistryError::ConversationDisplayCursorStale => "display_cursor_stale",
        HttpRegistryError::ConversationDisplayUnavailable => "conversation_display_unavailable",
        _ => "registry_error",
    };
    http_error_response(status, code, error.to_string())
}

fn session_catalog_batch_error_response(error: SessionCatalogBatchError) -> HttpResponse {
    match error {
        SessionCatalogBatchError::InvalidRequest(message) => {
            http_error_response(400, "invalid_session_batch_request", message)
        }
        SessionCatalogBatchError::StalePlan => http_error_response(
            409,
            "session_batch_plan_stale",
            "session catalog changed after the batch preview; review the selection again",
        ),
        SessionCatalogBatchError::Unavailable => http_error_response(
            503,
            "session_catalog_unavailable",
            "durable historical session catalog is unavailable",
        ),
    }
}

fn session_mutation_error_response(error: LocalSessionMutationError) -> HttpResponse {
    match error {
        LocalSessionMutationError::InvalidRequest => http_error_response(
            400,
            "invalid_session_mutation_request",
            "the conversation management request is invalid",
        ),
        LocalSessionMutationError::NotFound => http_error_response(
            404,
            "durable_session_not_found",
            "the saved conversation no longer exists",
        ),
        LocalSessionMutationError::NotReady => http_error_response(
            409,
            "durable_session_not_ready",
            "the saved conversation is not ready for this change",
        ),
        LocalSessionMutationError::IdentityChanged => http_error_response(
            409,
            "durable_session_identity_changed",
            "the saved conversation changed; refresh the list and try again",
        ),
        LocalSessionMutationError::Pinned => http_error_response(
            409,
            "durable_session_pinned",
            "unpin the saved conversation before deleting it",
        ),
        LocalSessionMutationError::Unavailable { source } => {
            eprintln!("session catalog mutation unavailable: {source:#}");
            http_error_response(
                503,
                "durable_session_mutation_unavailable",
                "the saved conversation could not be changed safely",
            )
        }
    }
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
    query: Option<String>,
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
