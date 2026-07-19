use std::{fmt, net::SocketAddr, sync::Arc, time::Duration};

use reqwest::{Client, RequestBuilder, Response, StatusCode, Url, header};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    dto::{
        DESKTOP_HTTP_PROTOCOL_VERSION, DesktopApprovalCommandReceipt,
        DesktopApprovalDecisionRequest, DesktopCatalogQuery, DesktopCommandEnvelope,
        DesktopErrorResponse, DesktopRunCancelCommandReceipt, DesktopRunCancelRequest,
        DesktopRunSnapshot, DesktopRunStartCommandReceipt, DesktopRunStartRequest,
        DesktopSessionCatalogPage, DesktopSessionCreateRequest, DesktopSessionListResponse,
        DesktopSessionOpenRequest, DesktopSessionSnapshot,
    },
    events::{DesktopProtocolEvent, DesktopProtocolEventClass, DesktopProtocolEventError},
    secret::DesktopBearerToken,
};

const MAX_JSON_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_SSE_FRAME_BYTES: usize = 2 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const RUN_EVENT_NAME: &str = "run_event";

/// Authenticated typed client for one desktop-owned loopback server.
///
/// The bearer and transport address are private and this type has no serialization surface.
#[derive(Clone)]
pub struct DesktopHttpClient {
    client: Client,
    address: SocketAddr,
    bearer: Arc<DesktopBearerToken>,
    client_id: Arc<str>,
}

impl DesktopHttpClient {
    pub(crate) fn new(
        client: Client,
        address: SocketAddr,
        bearer: Arc<DesktopBearerToken>,
    ) -> Self {
        Self {
            client,
            address,
            bearer,
            client_id: Arc::from(format!("sigil-desktop-{}", Uuid::new_v4())),
        }
    }

    /// Lists process-local session handles.
    pub async fn list_sessions(&self) -> Result<DesktopSessionListResponse, DesktopClientError> {
        self.get_json(self.route(["sessions"])?, StatusCode::OK)
            .await
    }

    /// Creates a new durable session through the server-owned runtime path.
    pub async fn create_session(
        &self,
        request: DesktopSessionCreateRequest,
    ) -> Result<DesktopSessionSnapshot, DesktopClientError> {
        self.post_json(self.route(["sessions"])?, &request, StatusCode::CREATED)
            .await
    }

    /// Revalidates and opens one durable catalog entry.
    pub async fn open_session(
        &self,
        request: DesktopSessionOpenRequest,
    ) -> Result<DesktopSessionSnapshot, DesktopClientError> {
        self.post_json(self.route(["sessions", "open"])?, &request, StatusCode::OK)
            .await
    }

    /// Queries one generation-consistent historical session page.
    pub async fn catalog(
        &self,
        query: &DesktopCatalogQuery,
    ) -> Result<DesktopSessionCatalogPage, DesktopClientError> {
        let mut url = self.route(["session-catalog"])?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(limit) = query.limit {
                pairs.append_pair("limit", &limit.to_string());
            }
            if let Some(cursor) = query.cursor.as_deref() {
                pairs.append_pair("cursor", cursor);
            }
            if let Some(value) = query.query.as_deref() {
                pairs.append_pair("q", value);
            }
            if let Some(provider) = query.provider.as_deref() {
                pairs.append_pair("provider", provider);
            }
            if let Some(pinned) = query.pinned {
                pairs.append_pair("pinned", if pinned { "true" } else { "false" });
            }
            if let Some(state) = query.state {
                pairs.append_pair(
                    "state",
                    match state {
                        crate::DesktopSessionCatalogState::Ready => "ready",
                        crate::DesktopSessionCatalogState::Oversized => "oversized",
                        crate::DesktopSessionCatalogState::ScanBudgetExceeded => {
                            "scan_budget_exceeded"
                        }
                        crate::DesktopSessionCatalogState::UnsupportedLegacy => {
                            "unsupported_legacy"
                        }
                        crate::DesktopSessionCatalogState::Invalid => "invalid",
                    },
                );
            }
        }
        self.get_json(url, StatusCode::OK).await
    }

    /// Reads one process-local session snapshot.
    pub async fn session(
        &self,
        session_id: &str,
    ) -> Result<DesktopSessionSnapshot, DesktopClientError> {
        self.get_json(self.route(["sessions", session_id])?, StatusCode::OK)
            .await
    }

    /// Starts a run with an idempotent command identity.
    pub async fn start_run(
        &self,
        session_id: &str,
        payload: DesktopRunStartRequest,
    ) -> Result<DesktopRunStartCommandReceipt, DesktopClientError> {
        let command = self.command(session_id, None, payload);
        self.post_json(
            self.route(["sessions", session_id, "runs"])?,
            &command,
            StatusCode::CREATED,
        )
        .await
    }

    /// Reads the latest server-owned run snapshot.
    pub async fn run(&self, run_id: &str) -> Result<DesktopRunSnapshot, DesktopClientError> {
        self.get_json(self.route(["runs", run_id])?, StatusCode::OK)
            .await
    }

    /// Connects to the server-owned replay-plus-live SSE stream for one run.
    pub async fn run_events(
        &self,
        session_id: &str,
        run_id: &str,
        last_event_id: Option<&str>,
    ) -> Result<DesktopRunEventStream, DesktopClientError> {
        validate_stream_identity(session_id)?;
        validate_stream_identity(run_id)?;
        if let Some(cursor) = last_event_id {
            validate_replay_cursor(cursor)?;
        }
        let mut request = self
            .client
            .get(self.route(["runs", run_id, "events"])?)
            .bearer_auth(self.bearer.expose())
            .header(header::ACCEPT, "text/event-stream");
        if let Some(cursor) = last_event_id {
            request = request.header("Last-Event-ID", cursor);
        }
        let response = request
            .send()
            .await
            .map_err(|_| DesktopClientError::RequestFailed)?;
        let status = response.status();
        if status != StatusCode::OK {
            return Err(rejected_response(response).await);
        }
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !content_type
            .split(';')
            .next()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
        {
            return Err(DesktopClientError::InvalidEventStream);
        }
        Ok(DesktopRunEventStream {
            response,
            buffer: Vec::new(),
            session_id: session_id.to_owned(),
            run_id: run_id.to_owned(),
            ended: false,
        })
    }

    /// Requests cooperative cancellation with an optimistic sequence guard.
    pub async fn cancel_run(
        &self,
        session_id: &str,
        run_id: &str,
        expected_stream_sequence: u64,
        payload: DesktopRunCancelRequest,
    ) -> Result<DesktopRunCancelCommandReceipt, DesktopClientError> {
        let command = self.command(session_id, Some(expected_stream_sequence), payload);
        self.post_json(
            self.route(["runs", run_id, "cancel"])?,
            &command,
            StatusCode::OK,
        )
        .await
    }

    /// Resolves one pending approval using the exact durable guard material.
    pub async fn resolve_approval(
        &self,
        session_id: &str,
        run_id: &str,
        call_id: &str,
        expected_stream_sequence: u64,
        payload: DesktopApprovalDecisionRequest,
    ) -> Result<DesktopApprovalCommandReceipt, DesktopClientError> {
        let command = self.command(session_id, Some(expected_stream_sequence), payload);
        self.post_json(
            self.route(["runs", run_id, "approvals", call_id])?,
            &command,
            StatusCode::OK,
        )
        .await
    }

    fn command<T>(
        &self,
        session_id: &str,
        expected_stream_sequence: Option<u64>,
        payload: T,
    ) -> DesktopCommandEnvelope<T> {
        DesktopCommandEnvelope {
            protocol_version: DESKTOP_HTTP_PROTOCOL_VERSION,
            command_id: format!("desktop-command-{}", Uuid::new_v4()),
            client_id: self.client_id.to_string(),
            session_id: session_id.to_owned(),
            expected_stream_sequence,
            correlation_id: None,
            payload,
        }
    }

    fn route<const N: usize>(&self, segments: [&str; N]) -> Result<Url, DesktopClientError> {
        let mut url = Url::parse(&format!("http://{}/", self.address))
            .map_err(|_| DesktopClientError::InvalidRoute)?;
        let mut path = url
            .path_segments_mut()
            .map_err(|_| DesktopClientError::InvalidRoute)?;
        path.clear();
        for segment in segments {
            if segment.is_empty() || segment.len() > 512 {
                return Err(DesktopClientError::InvalidRoute);
            }
            path.push(segment);
        }
        drop(path);
        Ok(url)
    }

    async fn get_json<T>(&self, url: Url, status: StatusCode) -> Result<T, DesktopClientError>
    where
        T: DeserializeOwned,
    {
        self.send_json(self.client.get(url), status).await
    }

    async fn post_json<T, B>(
        &self,
        url: Url,
        body: &B,
        status: StatusCode,
    ) -> Result<T, DesktopClientError>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.send_json(self.client.post(url).json(body), status)
            .await
    }

    async fn send_json<T>(
        &self,
        request: RequestBuilder,
        expected_status: StatusCode,
    ) -> Result<T, DesktopClientError>
    where
        T: DeserializeOwned,
    {
        let mut response = request
            .bearer_auth(self.bearer.expose())
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|_| DesktopClientError::RequestFailed)?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_JSON_RESPONSE_BYTES as u64)
        {
            return Err(DesktopClientError::ResponseTooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| DesktopClientError::RequestFailed)?
        {
            if body.len().saturating_add(chunk.len()) > MAX_JSON_RESPONSE_BYTES {
                return Err(DesktopClientError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        if status != expected_status {
            let code = serde_json::from_slice::<DesktopErrorResponse>(&body)
                .ok()
                .and_then(|error| safe_error_code(error.error.code));
            return Err(DesktopClientError::Rejected {
                status: status.as_u16(),
                code,
            });
        }
        serde_json::from_slice(&body).map_err(|_| DesktopClientError::InvalidResponse)
    }
}

/// Bounded incremental decoder for one authenticated run SSE response.
pub struct DesktopRunEventStream {
    response: Response,
    buffer: Vec<u8>,
    session_id: String,
    run_id: String,
    ended: bool,
}

impl DesktopRunEventStream {
    /// Returns the next protocol event, ignoring SSE comments and keep-alives.
    pub async fn next_event(&mut self) -> Result<Option<DesktopProtocolEvent>, DesktopClientError> {
        loop {
            if let Some(frame_end) = sse_frame_end(&self.buffer) {
                let frame = self.buffer.drain(..frame_end).collect::<Vec<_>>();
                let delimiter = if self.buffer.starts_with(b"\r\n\r\n") {
                    4
                } else {
                    2
                };
                self.buffer.drain(..delimiter);
                if let Some(event) = decode_sse_frame(&frame, &self.session_id, &self.run_id)? {
                    return Ok(Some(event));
                }
                continue;
            }
            if self.ended {
                return if self.buffer.is_empty() {
                    Ok(None)
                } else {
                    Err(DesktopClientError::InvalidEventStream)
                };
            }
            let next = self
                .response
                .chunk()
                .await
                .map_err(|_| DesktopClientError::RequestFailed)?;
            match next {
                Some(chunk) => {
                    if self.buffer.len().saturating_add(chunk.len()) > MAX_SSE_FRAME_BYTES {
                        return Err(DesktopClientError::ResponseTooLarge);
                    }
                    self.buffer.extend_from_slice(&chunk);
                }
                None => self.ended = true,
            }
        }
    }
}

impl fmt::Debug for DesktopRunEventStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopRunEventStream")
            .field("transport", &"authenticated loopback SSE")
            .field("buffered_bytes", &self.buffer.len())
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

fn sse_frame_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .or_else(|| buffer.windows(4).position(|window| window == b"\r\n\r\n"))
}

fn decode_sse_frame(
    frame: &[u8],
    session_id: &str,
    run_id: &str,
) -> Result<Option<DesktopProtocolEvent>, DesktopClientError> {
    let text = std::str::from_utf8(frame).map_err(|_| DesktopClientError::InvalidEventStream)?;
    let mut event_name = None;
    let mut event_id = None;
    let mut data = String::new();
    for raw_line in text.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" if event_name.replace(value).is_some() => {
                return Err(DesktopClientError::InvalidEventStream);
            }
            "id" if event_id.replace(value).is_some() => {
                return Err(DesktopClientError::InvalidEventStream);
            }
            "data" => {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
            "retry" => {}
            _ => {}
        }
    }
    if event_name.is_none() && event_id.is_none() && data.is_empty() {
        return Ok(None);
    }
    if event_name == Some("stream_gap") {
        return Err(DesktopClientError::EventStreamGap);
    }
    if event_name != Some(RUN_EVENT_NAME) || data.is_empty() {
        return Err(DesktopClientError::InvalidEventStream);
    }
    let event = serde_json::from_str::<DesktopProtocolEvent>(&data)
        .map_err(|_| DesktopClientError::InvalidEventStream)?;
    event
        .validate(session_id, run_id)
        .map_err(DesktopClientError::ProtocolEvent)?;
    match event.event_class {
        DesktopProtocolEventClass::Durable if event_id != event.replay_id.as_deref() => {
            Err(DesktopClientError::InvalidEventStream)
        }
        DesktopProtocolEventClass::Transient if event_id.is_some() => {
            Err(DesktopClientError::InvalidEventStream)
        }
        _ => Ok(Some(event)),
    }
}

fn validate_stream_identity(value: &str) -> Result<(), DesktopClientError> {
    if value.is_empty()
        || value.len() > 512
        || value.contains('/')
        || value.chars().any(char::is_control)
    {
        return Err(DesktopClientError::InvalidRoute);
    }
    Ok(())
}

fn validate_replay_cursor(value: &str) -> Result<(), DesktopClientError> {
    if value.is_empty() || value.len() > 4_096 || value.chars().any(char::is_control) {
        return Err(DesktopClientError::InvalidRoute);
    }
    Ok(())
}

async fn rejected_response(mut response: Response) -> DesktopClientError {
    let status = response.status();
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        if body.len().saturating_add(chunk.len()) > MAX_JSON_RESPONSE_BYTES {
            return DesktopClientError::ResponseTooLarge;
        }
        body.extend_from_slice(&chunk);
    }
    let code = serde_json::from_slice::<DesktopErrorResponse>(&body)
        .ok()
        .and_then(|error| safe_error_code(error.error.code));
    DesktopClientError::Rejected {
        status: status.as_u16(),
        code,
    }
}

impl fmt::Debug for DesktopHttpClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopHttpClient")
            .field("transport", &"loopback HTTP")
            .field("bearer", &"<redacted>")
            .finish_non_exhaustive()
    }
}

fn safe_error_code(code: String) -> Option<String> {
    (!code.is_empty()
        && code.len() <= 128
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then_some(code)
}

/// Path- and credential-free failures safe for native-shell projection.
#[derive(Debug, Error)]
pub enum DesktopClientError {
    #[error("desktop server route is invalid")]
    InvalidRoute,
    #[error("desktop server request failed")]
    RequestFailed,
    #[error("desktop server response exceeded its size limit")]
    ResponseTooLarge,
    #[error("desktop server returned HTTP {status}")]
    Rejected { status: u16, code: Option<String> },
    #[error("desktop server response is invalid")]
    InvalidResponse,
    #[error("desktop server event stream is invalid")]
    InvalidEventStream,
    #[error("desktop server event stream reported a live gap")]
    EventStreamGap,
    #[error(transparent)]
    ProtocolEvent(#[from] DesktopProtocolEventError),
}

#[cfg(test)]
#[path = "tests/client_tests.rs"]
mod tests;
