use std::{collections::BTreeMap, sync::Mutex};

use serde::{Deserialize, Serialize};
use sigil_kernel::{PublicRunEvent, PublicRunEventKind};
use thiserror::Error as ThisError;
use tokio::sync::broadcast;

/// SSE event name used for public run events.
pub const HTTP_RUN_EVENT_SSE_NAME: &str = "run_event";
/// Current schema version for HTTP protocol event envelopes.
pub const HTTP_PROTOCOL_EVENT_SCHEMA_VERSION: u32 = 1;

const HTTP_PROTOCOL_CURSOR_PREFIX: &str = "sigil-http-run-v1";

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
        | PublicRunEventKind::ToolCallArgsDelta { .. }
        | PublicRunEventKind::ToolProgress { .. } => HttpProtocolEventClass::Transient,
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
