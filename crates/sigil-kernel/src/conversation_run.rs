use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    event::{DurableEventType, EventClass},
    persistence::safe_persistence_text,
    secret::SecretRedactor,
    session::{JsonlSessionStore, SessionStreamRecord},
};

/// Durable schema version for provider-neutral foreground conversation-run lifecycle records.
pub const CONVERSATION_RUN_LIFECYCLE_SCHEMA_VERSION: u16 = 1;
/// Maximum persisted bytes for an adapter-owned conversation run identifier.
pub const MAX_CONVERSATION_RUN_ID_BYTES: usize = 256;
/// Maximum persisted bytes for a final assistant message identifier.
pub const MAX_CONVERSATION_RUN_MESSAGE_ID_BYTES: usize = 256;
/// Maximum persisted bytes for a redacted terminal summary.
pub const MAX_CONVERSATION_RUN_SUMMARY_BYTES: usize = 4 * 1024;

/// Provider-neutral terminal state for one foreground conversation run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum ConversationRunTerminalStatusV1 {
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    Blocked,
}

/// Recovery-critical start boundary for one adapter-owned foreground conversation run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationRunStartedEntryV1 {
    schema_version: u16,
    run_id: String,
    started_at_ms: u64,
}

impl ConversationRunStartedEntryV1 {
    /// Creates and validates a provider-neutral conversation-run start entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the run id is not a bounded persistence-safe identity or the
    /// timestamp is zero.
    pub fn new(run_id: impl Into<String>, started_at_ms: u64) -> Result<Self> {
        let entry = Self {
            schema_version: CONVERSATION_RUN_LIFECYCLE_SCHEMA_VERSION,
            run_id: run_id.into(),
            started_at_ms,
        };
        entry.validate_shape()?;
        Ok(entry)
    }

    #[must_use]
    pub fn schema_version(&self) -> u16 {
        self.schema_version
    }

    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    #[must_use]
    pub fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }

    /// Validates the durable entry independently of its event envelope.
    ///
    /// # Errors
    ///
    /// Returns an error when schema, identity, or timestamp invariants are violated.
    pub fn validate_shape(&self) -> Result<()> {
        validate_schema_version(self.schema_version)?;
        validate_stable_identity(
            "conversation run id",
            &self.run_id,
            MAX_CONVERSATION_RUN_ID_BYTES,
        )?;
        if self.started_at_ms == 0 {
            bail!("conversation run start timestamp must be non-zero");
        }
        Ok(())
    }
}

/// Recovery-critical terminal boundary for one adapter-owned foreground conversation run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationRunFinalizedEntryV1 {
    schema_version: u16,
    run_id: String,
    status: ConversationRunTerminalStatusV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    final_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    safe_summary: Option<String>,
    summary_truncated: bool,
    finalized_at_ms: u64,
}

impl ConversationRunFinalizedEntryV1 {
    /// Creates a terminal entry after redacting and bounding its optional diagnostic summary.
    ///
    /// The summary is not authoritative lifecycle state. It is retained only as bounded user-safe
    /// diagnostic copy; callers must provide the same redactor used for public adapter errors.
    ///
    /// # Errors
    ///
    /// Returns an error when identity, status, or timestamp invariants are violated.
    pub fn new(
        run_id: impl Into<String>,
        status: ConversationRunTerminalStatusV1,
        final_message_id: Option<String>,
        summary: Option<&str>,
        finalized_at_ms: u64,
        redactor: &SecretRedactor,
    ) -> Result<Self> {
        let (safe_summary, summary_truncated) = project_terminal_summary(summary, redactor);
        let entry = Self {
            schema_version: CONVERSATION_RUN_LIFECYCLE_SCHEMA_VERSION,
            run_id: run_id.into(),
            status,
            final_message_id,
            safe_summary,
            summary_truncated,
            finalized_at_ms,
        };
        entry.validate_shape()?;
        Ok(entry)
    }

    #[must_use]
    pub fn schema_version(&self) -> u16 {
        self.schema_version
    }

    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    #[must_use]
    pub fn status(&self) -> ConversationRunTerminalStatusV1 {
        self.status
    }

    #[must_use]
    pub fn final_message_id(&self) -> Option<&str> {
        self.final_message_id.as_deref()
    }

    #[must_use]
    pub fn safe_summary(&self) -> Option<&str> {
        self.safe_summary.as_deref()
    }

    #[must_use]
    pub fn summary_truncated(&self) -> bool {
        self.summary_truncated
    }

    #[must_use]
    pub fn finalized_at_ms(&self) -> u64 {
        self.finalized_at_ms
    }

    /// Validates the durable entry independently of its event envelope.
    ///
    /// # Errors
    ///
    /// Returns an error when schema, identity, summary, or timestamp invariants are violated.
    pub fn validate_shape(&self) -> Result<()> {
        validate_schema_version(self.schema_version)?;
        validate_stable_identity(
            "conversation run id",
            &self.run_id,
            MAX_CONVERSATION_RUN_ID_BYTES,
        )?;
        if let Some(final_message_id) = self.final_message_id.as_deref() {
            validate_stable_identity(
                "conversation run final message id",
                final_message_id,
                MAX_CONVERSATION_RUN_MESSAGE_ID_BYTES,
            )?;
        }
        if self.status == ConversationRunTerminalStatusV1::Succeeded
            && self.final_message_id.is_none()
        {
            bail!("succeeded conversation run requires a final message id");
        }
        match self.safe_summary.as_deref() {
            Some(summary) => {
                if summary.is_empty() {
                    bail!("conversation run safe summary must not be empty");
                }
                if summary.len() > MAX_CONVERSATION_RUN_SUMMARY_BYTES {
                    bail!(
                        "conversation run safe summary exceeds {MAX_CONVERSATION_RUN_SUMMARY_BYTES} bytes"
                    );
                }
                if safe_persistence_text(summary) != summary {
                    bail!("conversation run safe summary is not persistence-safe");
                }
            }
            None if self.summary_truncated => {
                bail!("conversation run cannot mark a missing summary as truncated");
            }
            None => {}
        }
        if self.finalized_at_ms == 0 {
            bail!("conversation run finalization timestamp must be non-zero");
        }
        Ok(())
    }

    fn has_same_terminal_intent(&self, other: &Self) -> bool {
        self.schema_version == other.schema_version
            && self.run_id == other.run_id
            && self.status == other.status
            && self.final_message_id == other.final_message_id
            && self.safe_summary == other.safe_summary
            && self.summary_truncated == other.summary_truncated
    }
}

/// Typed payload stored in the shared recovery-critical run lifecycle event categories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "record")]
pub enum ConversationRunLifecycleRecordV1 {
    ConversationRunStartedV1(ConversationRunStartedEntryV1),
    ConversationRunFinalizedV1(ConversationRunFinalizedEntryV1),
}

impl ConversationRunLifecycleRecordV1 {
    #[must_use]
    pub fn run_id(&self) -> &str {
        match self {
            Self::ConversationRunStartedV1(entry) => entry.run_id(),
            Self::ConversationRunFinalizedV1(entry) => entry.run_id(),
        }
    }

    /// Validates payload shape and the exact event category which carries it.
    ///
    /// # Errors
    ///
    /// Returns an error when a start record is stored as a terminal event or vice versa.
    pub fn validate_for_event_type(&self, event_type: DurableEventType) -> Result<()> {
        match self {
            Self::ConversationRunStartedV1(entry) => {
                entry.validate_shape()?;
                if event_type != DurableEventType::RunStatusChanged {
                    bail!("conversation run start must use run_status_changed");
                }
            }
            Self::ConversationRunFinalizedV1(entry) => {
                entry.validate_shape()?;
                if event_type != DurableEventType::RunFinalized {
                    bail!("conversation run finalization must use run_finalized");
                }
            }
        }
        Ok(())
    }
}

/// Cloneable durable recorder backed by one session's linear writer.
#[derive(Debug, Clone)]
pub struct ConversationRunLifecycleRecorder {
    store: JsonlSessionStore,
}

impl ConversationRunLifecycleRecorder {
    pub(crate) fn new(store: JsonlSessionStore) -> Self {
        Self { store }
    }

    /// Appends one start boundary exactly once.
    ///
    /// Returns `false` for an exact same-entry retry. A reused run id with different durable facts,
    /// or a stream containing an invalid conversation-run lifecycle, fails closed.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid shape, conflicting identity, or durable append failure.
    pub fn append_started(&self, entry: &ConversationRunStartedEntryV1) -> Result<bool> {
        entry.validate_shape()?;
        let entry = entry.clone();
        self.store.append_event_if(
            DurableEventType::RunStatusChanged,
            EventClass::Critical,
            serde_json::to_value(ConversationRunLifecycleRecordV1::ConversationRunStartedV1(
                entry.clone(),
            ))?,
            move |records| {
                let state = conversation_run_lifecycle_state(records)?;
                let Some(existing) = state.get(entry.run_id()) else {
                    return Ok(true);
                };
                match existing.started.as_ref() {
                    Some(started) if started == &entry => Ok(false),
                    Some(_) => bail!("conversation run id is reused with a conflicting start"),
                    None => bail!("conversation run terminal exists without a matching start"),
                }
            },
        )
    }

    /// Appends one terminal boundary exactly once after its matching start.
    ///
    /// Returns `false` for a retry of the same terminal intent. The first durable timestamp wins,
    /// so a caller can safely retry after an uncertain append without reproducing the original
    /// wall-clock value. Conflicting terminals and terminals without a matching start fail closed.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid shape, missing start, conflict, or durable append failure.
    pub fn append_finalized(&self, entry: &ConversationRunFinalizedEntryV1) -> Result<bool> {
        entry.validate_shape()?;
        let entry = entry.clone();
        self.store.append_event_if(
            DurableEventType::RunFinalized,
            EventClass::Critical,
            serde_json::to_value(
                ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(entry.clone()),
            )?,
            move |records| {
                let state = conversation_run_lifecycle_state(records)?;
                let Some(existing) = state.get(entry.run_id()) else {
                    bail!("conversation run finalization requires a matching durable start");
                };
                if existing.started.is_none() {
                    bail!("conversation run finalization requires a matching durable start");
                }
                match existing.finalized.as_ref() {
                    Some(finalized) if finalized.has_same_terminal_intent(&entry) => Ok(false),
                    Some(_) => bail!("conversation run id has a conflicting terminal"),
                    None => Ok(true),
                }
            },
        )
    }

    /// Closes one durable start left open by a previous process interruption.
    ///
    /// The caller must own the session's exclusive foreground admission while invoking this
    /// recovery step. Returns `false` when no unfinished run exists or when the same recovery
    /// terminal was already persisted by an uncertain earlier attempt.
    ///
    /// # Errors
    ///
    /// Returns an error when lifecycle order is invalid, more than one run is active, the recovery
    /// timestamp is invalid, or the terminal append conflicts or fails.
    pub fn reconcile_unfinished(&self, recovered_at_ms: u64) -> Result<bool> {
        if recovered_at_ms == 0 {
            bail!("conversation run recovery timestamp must be non-zero");
        }
        let records = JsonlSessionStore::read_event_records(self.store.path())?;
        let Some(started) = active_conversation_run(&records)? else {
            return Ok(false);
        };
        let terminal = ConversationRunFinalizedEntryV1::new(
            started.run_id(),
            ConversationRunTerminalStatusV1::Interrupted,
            None,
            Some("run interrupted before durable terminal recovery"),
            recovered_at_ms,
            &SecretRedactor::empty(),
        )?;
        self.append_finalized(&terminal)
    }
}

/// Decodes one typed conversation-run lifecycle record from its durable V2 envelope.
///
/// Existing kernel run lifecycle and cancellation payloads remain outside this contract. An
/// unrecognized payload in these recovery-critical event categories fails closed.
///
/// # Errors
///
/// Returns an error for checksum failure, unknown critical events, malformed tagged payloads,
/// phase/event mismatch, or unknown critical run lifecycle tags.
pub fn conversation_run_lifecycle_record_from_stream(
    record: &SessionStreamRecord,
) -> Result<Option<ConversationRunLifecycleRecordV1>> {
    let _ = record.domain_event_record()?;
    let event = record.stored_event();
    let Some(event_type) = event.event_kind() else {
        return Ok(None);
    };
    if !matches!(
        event_type,
        DurableEventType::RunStatusChanged | DurableEventType::RunFinalized
    ) {
        return Ok(None);
    }

    match event.payload.get("record") {
        Some(Value::String(tag)) => match tag.as_str() {
            "conversation_run_started_v1" | "conversation_run_finalized_v1" => {
                let lifecycle: ConversationRunLifecycleRecordV1 =
                    serde_json::from_value(event.payload.clone())
                        .context("failed to decode conversation run lifecycle payload")?;
                lifecycle.validate_for_event_type(event_type)?;
                Ok(Some(lifecycle))
            }
            "requested" if event_type == DurableEventType::RunStatusChanged => Ok(None),
            "finalized" if event_type == DurableEventType::RunFinalized => Ok(None),
            "requested" | "finalized" => {
                bail!("run cancellation lifecycle payload uses the wrong event category")
            }
            _ => bail!("unknown critical run lifecycle record {tag}"),
        },
        Some(_) => bail!("critical run lifecycle record tag must be a string"),
        None => {
            decode_current_kernel_run_lifecycle_payload(&event.payload)?;
            Ok(None)
        }
    }
}

#[derive(Default)]
struct ConversationRunLifecycleState {
    started: Option<ConversationRunStartedEntryV1>,
    finalized: Option<ConversationRunFinalizedEntryV1>,
}

fn conversation_run_lifecycle_state(
    records: &[SessionStreamRecord],
) -> Result<BTreeMap<String, ConversationRunLifecycleState>> {
    let mut state = BTreeMap::<String, ConversationRunLifecycleState>::new();
    for record in records {
        let Some(record) = conversation_run_lifecycle_record_from_stream(record)? else {
            continue;
        };
        match record {
            ConversationRunLifecycleRecordV1::ConversationRunStartedV1(entry) => {
                let run = state.entry(entry.run_id().to_owned()).or_default();
                if run.started.replace(entry).is_some() {
                    bail!("conversation run stream contains duplicate starts");
                }
            }
            ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(entry) => {
                let run = state.entry(entry.run_id().to_owned()).or_default();
                if run.started.is_none() {
                    bail!("conversation run stream contains a terminal without a matching start");
                }
                if run.finalized.replace(entry).is_some() {
                    bail!("conversation run stream contains duplicate terminals");
                }
            }
        }
    }
    Ok(state)
}

fn active_conversation_run(
    records: &[SessionStreamRecord],
) -> Result<Option<ConversationRunStartedEntryV1>> {
    let mut active: Option<ConversationRunStartedEntryV1> = None;
    for record in records {
        let Some(record) = conversation_run_lifecycle_record_from_stream(record)? else {
            continue;
        };
        match record {
            ConversationRunLifecycleRecordV1::ConversationRunStartedV1(started) => {
                if active.is_some() {
                    bail!("conversation run stream contains overlapping active runs");
                }
                active = Some(started);
            }
            ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(finalized) => {
                let Some(started) = active.as_ref() else {
                    bail!("conversation run stream contains a terminal without an active start");
                };
                if started.run_id() != finalized.run_id() {
                    bail!("conversation run terminal belongs to another active run");
                }
                active = None;
            }
        }
    }
    Ok(active)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentKernelRunLifecyclePayload {
    run_status: String,
    terminal_reason: String,
    final_message_id: Option<String>,
    tool_calls: usize,
    error: Option<String>,
}

fn decode_current_kernel_run_lifecycle_payload(payload: &Value) -> Result<()> {
    let payload: CurrentKernelRunLifecyclePayload = serde_json::from_value(payload.clone())
        .context("unrecognized critical run lifecycle payload")?;
    if payload.run_status.trim().is_empty() || payload.terminal_reason.trim().is_empty() {
        bail!("current kernel run lifecycle payload has an empty status or terminal reason");
    }
    let _ = (payload.final_message_id, payload.tool_calls, payload.error);
    Ok(())
}

fn project_terminal_summary(
    summary: Option<&str>,
    redactor: &SecretRedactor,
) -> (Option<String>, bool) {
    let Some(summary) = summary else {
        return (None, false);
    };
    let safe = safe_persistence_text(&redactor.redact_text(summary));
    if safe.is_empty() {
        return (None, false);
    }
    let (safe, truncated) = truncate_utf8_bytes(&safe, MAX_CONVERSATION_RUN_SUMMARY_BYTES);
    (Some(safe), truncated)
}

fn truncate_utf8_bytes(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

fn validate_schema_version(schema_version: u16) -> Result<()> {
    if schema_version != CONVERSATION_RUN_LIFECYCLE_SCHEMA_VERSION {
        bail!("unsupported conversation run lifecycle schema version {schema_version}");
    }
    Ok(())
}

fn validate_stable_identity(label: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.is_empty() {
        bail!("{label} must not be empty");
    }
    if value.len() > max_bytes {
        bail!("{label} exceeds {max_bytes} bytes");
    }
    if value.bytes().any(|byte| {
        !matches!(
            byte,
            b'a'..=b'z'
                | b'A'..=b'Z'
                | b'0'..=b'9'
                | b'-'
                | b'_'
                | b'.'
                | b':'
        )
    }) {
        bail!("{label} contains unsupported characters");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/conversation_run_tests.rs"]
mod tests;
