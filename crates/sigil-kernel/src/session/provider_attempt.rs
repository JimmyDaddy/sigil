use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::*;
use crate::{EventId, SessionId, projection_apply_decision};

/// Schema version for the provider physical-attempt direct payloads.
pub const PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION: u16 = 2;

/// Projection schema version for provider physical attempts.
pub const PROVIDER_PHYSICAL_ATTEMPT_PROJECTION_SCHEMA_VERSION: u16 = 1;

/// Maximum durable output references carried by one physical-attempt terminal.
pub const MAX_PROVIDER_PHYSICAL_ATTEMPT_OUTPUT_REFS: usize = 64;

/// Maximum durable side-effect references carried by one physical-attempt terminal.
pub const MAX_PROVIDER_PHYSICAL_ATTEMPT_SIDE_EFFECT_REFS: usize = 64;

/// Maximum aggregate byte size of terminal reference identities.
pub const MAX_PROVIDER_PHYSICAL_ATTEMPT_REFERENCE_BYTES: usize = 16 * 1024;

/// Stable identity of one provider physical attempt.
pub type ProviderPhysicalAttemptId = String;

/// Why a provider physical attempt was issued.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderPhysicalAttemptPurpose {
    ConversationGeneration,
    SemanticCompaction,
    NativeCompaction,
    /// Provider-owned measurement that cannot generate a model response or invoke tools.
    InputTokenMeasurement,
}

/// Recovery-critical record written and synced before provider I/O begins.
///
/// The request fingerprint currently binds the frozen provider-neutral material introduced by
/// K25.6A. Provider wire/token/profile evidence is deliberately added by K25.7 rather than
/// claiming byte-for-byte provider-wire identity before that proof exists.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderPhysicalAttemptStartedEntry {
    pub schema_version: u16,
    pub physical_attempt_id: ProviderPhysicalAttemptId,
    pub logical_run_id: String,
    pub purpose: ProviderPhysicalAttemptPurpose,
    pub request_material_fingerprint: String,
    pub provider_name: String,
    pub model_name: String,
    pub started_at_unix_ms: u64,
}

impl ProviderPhysicalAttemptStartedEntry {
    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.schema_version != PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION {
            bail!(
                "unsupported provider physical-attempt schema version {}",
                self.schema_version
            );
        }
        validate_identity("provider physical attempt id", &self.physical_attempt_id)?;
        validate_identity("provider logical run id", &self.logical_run_id)?;
        validate_identity(
            "provider request material fingerprint",
            &self.request_material_fingerprint,
        )?;
        validate_label("provider name", &self.provider_name)?;
        validate_label("provider model name", &self.model_name)?;
        Ok(())
    }
}

/// Outcome recorded exactly once after a provider physical attempt reaches a known boundary.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderPhysicalAttemptOutcome {
    Completed,
    ConfirmedNoModelConsumption,
    FailedAfterOutputOrSideEffect,
    ProtocolRejectedAfterOutput,
    TransportOutcomeUncertain,
    Interrupted,
}

/// Recovery-critical terminal for one provider physical attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderPhysicalAttemptTerminalEntry {
    pub schema_version: u16,
    pub physical_attempt_id: ProviderPhysicalAttemptId,
    pub request_material_fingerprint: String,
    pub outcome: ProviderPhysicalAttemptOutcome,
    /// An exact provider-declared pre-generation rejection, if one was proven.
    pub rejection: Option<crate::ProviderRequestRejection>,
    pub provider_request_id: Option<String>,
    pub provider_response_id: Option<String>,
    pub durable_output_event_ids: Vec<EventId>,
    pub durable_side_effect_event_ids: Vec<EventId>,
    pub finished_at_unix_ms: u64,
}

impl ProviderPhysicalAttemptTerminalEntry {
    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.schema_version != PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION {
            bail!(
                "unsupported provider physical-attempt schema version {}",
                self.schema_version
            );
        }
        validate_identity("provider physical attempt id", &self.physical_attempt_id)?;
        validate_identity(
            "provider request material fingerprint",
            &self.request_material_fingerprint,
        )?;
        if let Some(provider_request_id) = &self.provider_request_id {
            validate_identity("provider request id", provider_request_id)?;
        }
        if let Some(provider_response_id) = &self.provider_response_id {
            validate_identity("provider response id", provider_response_id)?;
        }
        if self.rejection.is_some()
            && self.outcome != ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption
        {
            bail!("provider physical-attempt rejection requires confirmed no model consumption");
        }
        validate_reference_ids(
            "provider durable output event ids",
            &self.durable_output_event_ids,
            MAX_PROVIDER_PHYSICAL_ATTEMPT_OUTPUT_REFS,
        )?;
        validate_reference_ids(
            "provider durable side-effect event ids",
            &self.durable_side_effect_event_ids,
            MAX_PROVIDER_PHYSICAL_ATTEMPT_SIDE_EFFECT_REFS,
        )?;
        let reference_bytes = self
            .durable_output_event_ids
            .iter()
            .chain(self.durable_side_effect_event_ids.iter())
            .map(String::len)
            .sum::<usize>();
        if reference_bytes > MAX_PROVIDER_PHYSICAL_ATTEMPT_REFERENCE_BYTES {
            bail!("provider physical-attempt terminal references exceed byte limit");
        }
        if self.rejection.is_some()
            && (!self.durable_output_event_ids.is_empty()
                || !self.durable_side_effect_event_ids.is_empty())
        {
            bail!(
                "provider physical-attempt rejection cannot carry output or side-effect references"
            );
        }
        let mut referenced = std::collections::BTreeSet::new();
        for event_id in self
            .durable_output_event_ids
            .iter()
            .chain(self.durable_side_effect_event_ids.iter())
        {
            if !referenced.insert(event_id) {
                bail!("provider physical-attempt terminal references overlap");
            }
        }
        Ok(())
    }
}

/// Reconstructed state of one provider physical attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderPhysicalAttemptState {
    session_id: SessionId,
    pub started_event_id: EventId,
    pub started_stream_sequence: u64,
    pub entry: ProviderPhysicalAttemptStartedEntry,
    pub terminal_event_id: Option<EventId>,
    pub terminal_stream_sequence: Option<u64>,
    pub terminal: Option<ProviderPhysicalAttemptTerminalEntry>,
    causal_output_or_side_effect_event_ids: Vec<EventId>,
    last_causation_event_id: EventId,
}

impl ProviderPhysicalAttemptState {
    fn is_terminal(&self) -> bool {
        self.terminal.is_some()
    }

    /// Returns the durable session scope that owns this physical attempt.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// Read-only provider physical-attempt projection reconstructed from the V2 event stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderPhysicalAttemptProjection {
    cursor: Option<ProjectionCursor>,
    attempts: BTreeMap<ProviderPhysicalAttemptId, ProviderPhysicalAttemptState>,
}

impl ProviderPhysicalAttemptProjection {
    /// Rebuilds the attempt lifecycle and validates every matching causal output chain.
    ///
    /// # Errors
    ///
    /// Returns an error when an attempt has duplicate starts or terminals, inconsistent request
    /// material, malformed references, or a broken output causation chain.
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let mut projection = Self::default();
        for record in records {
            projection.apply_record(record)?;
        }
        Ok(projection)
    }

    /// Returns the projection cursor after the last consumed record.
    #[must_use]
    pub fn cursor(&self) -> Option<&ProjectionCursor> {
        self.cursor.as_ref()
    }

    /// Returns one physical attempt by its payload identity.
    #[must_use]
    pub fn attempt(&self, attempt_id: &str) -> Option<&ProviderPhysicalAttemptState> {
        self.attempts.get(attempt_id)
    }

    /// Returns every attempt with one durable logical-run correlation id.
    ///
    /// Callers that require a one-to-one handoff must reject zero or multiple matches rather than
    /// guessing which provider request consumed a queued input.
    #[must_use]
    pub fn attempts_for_logical_run_id(
        &self,
        logical_run_id: &str,
    ) -> Vec<&ProviderPhysicalAttemptState> {
        self.attempts
            .values()
            .filter(|attempt| attempt.entry.logical_run_id == logical_run_id)
            .collect()
    }

    /// Returns every physical attempt that was sent but has no durable terminal.
    #[must_use]
    pub fn unfinished_attempts(&self) -> Vec<&ProviderPhysicalAttemptState> {
        self.attempts
            .values()
            .filter(|attempt| !attempt.is_terminal())
            .collect()
    }

    fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        let event = record.stored_event();
        let decision = projection_apply_decision(self.cursor.as_ref(), event)?;
        if decision == ProjectionApplyDecision::IgnoreAlreadyApplied {
            return Ok(());
        }
        match decode_stored_event(event.clone())? {
            StoredEventDecode::Known(_) | StoredEventDecode::UnknownNonCritical(_) => {}
        }

        match event.event_kind() {
            Some(DurableEventType::ProviderPhysicalAttemptStarted) => {
                let entry: ProviderPhysicalAttemptStartedEntry = decode_attempt_payload(event)?;
                self.apply_started(event, entry)?;
            }
            Some(DurableEventType::ProviderPhysicalAttemptTerminal) => {
                let entry: ProviderPhysicalAttemptTerminalEntry = decode_attempt_payload(event)?;
                self.apply_terminal(event, entry)?;
            }
            Some(_) | None => self.apply_output_event(event)?,
        }

        self.cursor =
            Some(record.projection_cursor(PROVIDER_PHYSICAL_ATTEMPT_PROJECTION_SCHEMA_VERSION));
        Ok(())
    }

    fn apply_started(
        &mut self,
        event: &StoredEvent,
        entry: ProviderPhysicalAttemptStartedEntry,
    ) -> Result<()> {
        entry.validate_shape()?;
        if event.correlation_id.as_deref() != Some(event.event_id.as_str()) {
            bail!("provider physical-attempt start correlation id must equal its event id");
        }
        if event.causation_id.is_some() {
            bail!("provider physical-attempt start must not have a causation id");
        }
        if self.attempts.contains_key(&entry.physical_attempt_id) {
            bail!(
                "provider physical attempt {} was started more than once",
                entry.physical_attempt_id
            );
        }
        self.attempts.insert(
            entry.physical_attempt_id.clone(),
            ProviderPhysicalAttemptState {
                session_id: event.session_id.clone(),
                started_event_id: event.event_id.clone(),
                started_stream_sequence: event.stream_sequence,
                entry,
                terminal_event_id: None,
                terminal_stream_sequence: None,
                terminal: None,
                causal_output_or_side_effect_event_ids: Vec::new(),
                last_causation_event_id: event.event_id.clone(),
            },
        );
        Ok(())
    }

    fn apply_output_event(&mut self, event: &StoredEvent) -> Result<()> {
        let Some(correlation_id) = event.correlation_id.as_deref() else {
            return Ok(());
        };
        let Some(attempt) = self
            .attempts
            .values_mut()
            .find(|attempt| attempt.started_event_id == correlation_id)
        else {
            return Ok(());
        };
        if attempt.is_terminal() {
            return Ok(());
        }
        if event.session_id != attempt.session_id {
            bail!("provider physical-attempt output belongs to a different session");
        }
        if event.stream_sequence <= attempt.started_stream_sequence {
            bail!("provider physical-attempt output precedes its start");
        }
        if event.causation_id.as_deref() != Some(attempt.last_causation_event_id.as_str()) {
            bail!("provider physical-attempt output causation does not follow its attempt chain");
        }
        attempt
            .causal_output_or_side_effect_event_ids
            .push(event.event_id.clone());
        attempt.last_causation_event_id = event.event_id.clone();
        Ok(())
    }

    fn apply_terminal(
        &mut self,
        event: &StoredEvent,
        entry: ProviderPhysicalAttemptTerminalEntry,
    ) -> Result<()> {
        entry.validate_shape()?;
        let attempt = self
            .attempts
            .get_mut(&entry.physical_attempt_id)
            .with_context(|| {
                format!(
                    "provider physical-attempt terminal references unknown attempt {}",
                    entry.physical_attempt_id
                )
            })?;
        if attempt.is_terminal() {
            bail!(
                "provider physical attempt {} already has a terminal event",
                entry.physical_attempt_id
            );
        }
        if attempt.entry.request_material_fingerprint != entry.request_material_fingerprint {
            bail!("provider physical-attempt terminal fingerprint does not match its start");
        }
        if event.session_id != attempt.session_id {
            bail!("provider physical-attempt terminal belongs to a different session");
        }
        if event.correlation_id.as_deref() != Some(attempt.started_event_id.as_str()) {
            bail!("provider physical-attempt terminal correlation does not match its start");
        }
        if event.causation_id.as_deref() != Some(attempt.last_causation_event_id.as_str()) {
            bail!("provider physical-attempt terminal causation does not follow output chain");
        }
        let referenced_event_ids = entry
            .durable_output_event_ids
            .iter()
            .chain(entry.durable_side_effect_event_ids.iter())
            .cloned()
            .collect::<Vec<_>>();
        if referenced_event_ids != attempt.causal_output_or_side_effect_event_ids {
            bail!(
                "provider physical-attempt terminal output references are incomplete or unordered"
            );
        }
        attempt.terminal_event_id = Some(event.event_id.clone());
        attempt.terminal_stream_sequence = Some(event.stream_sequence);
        attempt.terminal = Some(entry);
        Ok(())
    }
}

/// In-process recorder for one agent provider turn.
///
/// Store-backed sessions receive a synced start barrier before provider I/O. In-memory sessions
/// retain existing behavior and deliberately do not manufacture non-resumable audit facts.
pub(crate) enum ProviderPhysicalAttemptAudit {
    InMemory,
    Durable(DurableProviderPhysicalAttemptAudit),
}

pub(crate) struct DurableProviderPhysicalAttemptAudit {
    physical_attempt_id: ProviderPhysicalAttemptId,
    request_material_fingerprint: String,
    start_event_id: EventId,
    last_causation_event_id: EventId,
    durable_output_event_ids: Vec<EventId>,
    terminal_recorded: bool,
}

/// Durable guard for one provider request that performs no model generation.
///
/// The measurement request still receives the same start/terminal audit barrier as a
/// conversation request, so a process loss cannot be mistaken for permission to retry remote
/// work. It deliberately cannot append model-visible output or side-effect references.
pub struct ProviderNonGeneratingAttempt {
    audit: ProviderPhysicalAttemptAudit,
    receipt: Option<ProviderNonGeneratingAttemptReceipt>,
    completed: bool,
}

/// Process-local identity of one completed non-generating provider attempt.
///
/// This receipt never enters the session stream. A caller can use it only to bind an immediate
/// post-measurement admission to the exact durable start/terminal pair; it is not a retry token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderNonGeneratingAttemptReceipt {
    session_scope_id: String,
    physical_attempt_id: ProviderPhysicalAttemptId,
    request_material_fingerprint: String,
    purpose: ProviderPhysicalAttemptPurpose,
}

impl ProviderNonGeneratingAttemptReceipt {
    /// Returns the session scope that owns this attempt.
    #[must_use]
    pub fn session_scope_id(&self) -> &str {
        &self.session_scope_id
    }

    /// Returns the durable physical-attempt identity.
    #[must_use]
    pub fn physical_attempt_id(&self) -> &str {
        &self.physical_attempt_id
    }

    /// Returns the fingerprint bound to the measured frozen request.
    #[must_use]
    pub fn request_material_fingerprint(&self) -> &str {
        &self.request_material_fingerprint
    }

    /// Returns the only permitted non-generating attempt purpose.
    #[must_use]
    pub fn purpose(&self) -> ProviderPhysicalAttemptPurpose {
        self.purpose
    }
}

impl ProviderNonGeneratingAttempt {
    /// Starts a synced non-generating provider physical attempt before provider I/O.
    ///
    /// # Errors
    ///
    /// Returns an error when the purpose is generation, the request identity is invalid, or the
    /// durable start barrier cannot be confirmed.
    pub async fn start(
        session: &Session,
        logical_run_id: &str,
        frozen_request: &crate::FrozenProviderRequestMaterial,
        purpose: ProviderPhysicalAttemptPurpose,
    ) -> Result<Self> {
        if purpose != ProviderPhysicalAttemptPurpose::InputTokenMeasurement {
            bail!("non-generating provider attempt requires input token measurement purpose");
        }
        let audit = ProviderPhysicalAttemptAudit::start_with_purpose(
            session,
            logical_run_id,
            frozen_request,
            purpose,
        )
        .await?;
        let receipt = audit.non_generating_receipt(session.session_scope_id(), purpose);
        Ok(Self {
            audit,
            receipt,
            completed: false,
        })
    }

    /// Writes this attempt's only terminal after its remote measurement finishes.
    ///
    /// # Errors
    ///
    /// Returns an error when the durable terminal cannot be appended or has already been
    /// recorded.
    pub async fn finish(
        &mut self,
        session: &Session,
        outcome: ProviderPhysicalAttemptOutcome,
    ) -> Result<()> {
        self.audit.finish(session, outcome, None).await?;
        self.completed = outcome == ProviderPhysicalAttemptOutcome::Completed;
        Ok(())
    }

    /// Returns this exact durable receipt only after a completed terminal was written
    /// successfully.
    #[must_use]
    pub fn completed_receipt(&self) -> Option<&ProviderNonGeneratingAttemptReceipt> {
        self.completed.then_some(self.receipt.as_ref()).flatten()
    }
}

impl ProviderPhysicalAttemptAudit {
    fn non_generating_receipt(
        &self,
        session_scope_id: &str,
        purpose: ProviderPhysicalAttemptPurpose,
    ) -> Option<ProviderNonGeneratingAttemptReceipt> {
        let Self::Durable(audit) = self else {
            return None;
        };
        Some(ProviderNonGeneratingAttemptReceipt {
            session_scope_id: session_scope_id.to_owned(),
            physical_attempt_id: audit.physical_attempt_id.clone(),
            request_material_fingerprint: audit.request_material_fingerprint.clone(),
            purpose,
        })
    }

    pub(crate) async fn start(
        session: &Session,
        logical_run_id: &str,
        frozen_request: &crate::FrozenProviderRequestMaterial,
    ) -> Result<Self> {
        Self::start_with_purpose(
            session,
            logical_run_id,
            frozen_request,
            ProviderPhysicalAttemptPurpose::ConversationGeneration,
        )
        .await
    }

    async fn start_with_purpose(
        session: &Session,
        logical_run_id: &str,
        frozen_request: &crate::FrozenProviderRequestMaterial,
        purpose: ProviderPhysicalAttemptPurpose,
    ) -> Result<Self> {
        let Some(store) = session.durable_store() else {
            return Ok(Self::InMemory);
        };
        let request = frozen_request.request();
        let physical_attempt_id = format!("provider-attempt-{}", Uuid::new_v4());
        let start_event_id = Uuid::new_v4().to_string();
        let entry = ProviderPhysicalAttemptStartedEntry {
            schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
            physical_attempt_id: physical_attempt_id.clone(),
            logical_run_id: logical_run_id.to_owned(),
            purpose,
            request_material_fingerprint: frozen_request.fingerprint().to_owned(),
            provider_name: request.provider_name.clone(),
            model_name: request.model_name.clone(),
            started_at_unix_ms: unix_time_ms(),
        };
        entry.validate_shape()?;
        append_direct_record_and_sync(
            store,
            session.session_scope_id().to_owned(),
            DurableEventType::ProviderPhysicalAttemptStarted,
            serde_json::to_value(&entry)
                .context("failed to encode provider physical-attempt start")?,
            physical_attempt_id.clone(),
            start_event_id.clone(),
            Some(start_event_id.clone()),
            None,
            PhysicalAttemptAppendGuard::Start {
                physical_attempt_id: physical_attempt_id.clone(),
            },
        )
        .await?;
        Ok(Self::Durable(DurableProviderPhysicalAttemptAudit {
            physical_attempt_id,
            request_material_fingerprint: entry.request_material_fingerprint,
            start_event_id: start_event_id.clone(),
            last_causation_event_id: start_event_id,
            durable_output_event_ids: Vec::new(),
            terminal_recorded: false,
        }))
    }

    pub(crate) async fn append_output_control(
        &mut self,
        session: &mut Session,
        control: ControlEntry,
    ) -> Result<()> {
        let Self::Durable(audit) = self else {
            return session.append_control(control);
        };
        if audit.terminal_recorded {
            bail!("provider physical-attempt output cannot follow its terminal");
        }
        let store = session
            .durable_store()
            .context("provider physical-attempt audit is missing its durable store")?;
        let entry = SessionLogEntry::Control(control.clone());
        let event_type = session_entry_event_type(&entry);
        let payload = serde_json::json!({ "session_log_entry": entry });
        let event_id = Uuid::new_v4().to_string();
        let expected_attempt_id = audit.physical_attempt_id.clone();
        let expected_start_event_id = audit.start_event_id.clone();
        let expected_causation_id = audit.last_causation_event_id.clone();
        let appended = tokio::task::spawn_blocking(move || {
            store.append_event_if_with_identity(
                event_type,
                payload,
                event_id,
                Some(expected_start_event_id.clone()),
                Some(expected_causation_id.clone()),
                |records| {
                    let projection = ProviderPhysicalAttemptProjection::from_records(records)?;
                    let attempt = projection.attempt(&expected_attempt_id).with_context(|| {
                        format!(
                            "provider output references missing physical attempt {expected_attempt_id}"
                        )
                    })?;
                    if attempt.is_terminal() {
                        bail!("provider output cannot be appended after its physical terminal");
                    }
                    if attempt.started_event_id != expected_start_event_id {
                        bail!("provider output start identity does not match physical attempt");
                    }
                    if attempt.last_causation_event_id != expected_causation_id {
                        bail!("provider output causation does not match physical attempt chain");
                    }
                    Ok(true)
                },
            )
        })
        .await
        .context("provider output durable append task failed")??
        .context("provider output durable append was not attempted")?;
        audit.last_causation_event_id = appended.event_id.clone();
        audit.durable_output_event_ids.push(appended.event_id);
        session.record_durably_appended_control(control);
        Ok(())
    }

    pub(crate) async fn finish(
        &mut self,
        session: &Session,
        outcome: ProviderPhysicalAttemptOutcome,
        rejection: Option<crate::ProviderRequestRejection>,
    ) -> Result<()> {
        let Self::Durable(audit) = self else {
            return Ok(());
        };
        if audit.terminal_recorded {
            bail!("provider physical-attempt terminal was already recorded");
        }
        let entry = ProviderPhysicalAttemptTerminalEntry {
            schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
            physical_attempt_id: audit.physical_attempt_id.clone(),
            request_material_fingerprint: audit.request_material_fingerprint.clone(),
            outcome,
            rejection,
            provider_request_id: None,
            provider_response_id: None,
            durable_output_event_ids: audit.durable_output_event_ids.clone(),
            durable_side_effect_event_ids: Vec::new(),
            finished_at_unix_ms: unix_time_ms(),
        };
        entry.validate_shape()?;
        let store = session
            .durable_store()
            .context("provider physical-attempt audit is missing its durable store")?;
        append_direct_record_and_sync(
            store,
            session.session_scope_id().to_owned(),
            DurableEventType::ProviderPhysicalAttemptTerminal,
            serde_json::to_value(&entry)
                .context("failed to encode provider physical-attempt terminal")?,
            audit.physical_attempt_id.clone(),
            Uuid::new_v4().to_string(),
            Some(audit.start_event_id.clone()),
            Some(audit.last_causation_event_id.clone()),
            PhysicalAttemptAppendGuard::Terminal {
                entry: entry.clone(),
                start_event_id: audit.start_event_id.clone(),
                causation_event_id: audit.last_causation_event_id.clone(),
            },
        )
        .await?;
        audit.terminal_recorded = true;
        Ok(())
    }

    #[must_use]
    pub(crate) fn has_durable_output_or_side_effect(&self) -> bool {
        matches!(self, Self::Durable(audit) if !audit.durable_output_event_ids.is_empty())
    }
}

impl JsonlSessionStore {
    /// Appends one interrupted terminal for each provider request that crossed the send barrier
    /// but did not reach a durable terminal before process loss.
    ///
    /// This is intentionally an explicit single-writer recovery operation; normal projection and
    /// `Session::load_from_store` remain read-only with respect to this lifecycle.
    pub fn recover_unfinished_provider_physical_attempts(&self, now_unix_ms: u64) -> Result<usize> {
        let records = self.read_event_records_writer()?;
        let projection = ProviderPhysicalAttemptProjection::from_records(&records)?;
        let unfinished = projection
            .unfinished_attempts()
            .into_iter()
            .map(|attempt| {
                (
                    attempt.entry.clone(),
                    attempt.started_event_id.clone(),
                    attempt.last_causation_event_id.clone(),
                    attempt.causal_output_or_side_effect_event_ids.clone(),
                )
            })
            .collect::<Vec<_>>();
        let session_id =
            stream_session_id(&records).unwrap_or_else(|| session_id_for_path(self.path()));
        let mut appended = 0usize;
        for (started, start_event_id, causation_id, output_event_ids) in unfinished {
            let entry = ProviderPhysicalAttemptTerminalEntry {
                schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
                physical_attempt_id: started.physical_attempt_id.clone(),
                request_material_fingerprint: started.request_material_fingerprint.clone(),
                outcome: ProviderPhysicalAttemptOutcome::Interrupted,
                rejection: None,
                provider_request_id: None,
                provider_response_id: None,
                durable_output_event_ids: output_event_ids,
                durable_side_effect_event_ids: Vec::new(),
                finished_at_unix_ms: now_unix_ms,
            };
            entry.validate_shape()?;
            let event_id = stable_event_uuid(
                "sigil-provider-physical-attempt-recovery-terminal",
                &format!("{session_id}:{}:interrupted", entry.physical_attempt_id),
            );
            let payload = serde_json::to_value(&entry)
                .context("failed to encode interrupted provider physical-attempt terminal")?;
            let appended_event = self.append_event_if_with_identity(
                DurableEventType::ProviderPhysicalAttemptTerminal,
                payload,
                event_id,
                Some(start_event_id.clone()),
                Some(causation_id.clone()),
                |records| {
                    let projection = ProviderPhysicalAttemptProjection::from_records(records)?;
                    let attempt = projection
                        .attempt(&started.physical_attempt_id)
                        .with_context(|| {
                            format!(
                                "provider recovery references missing physical attempt {}",
                                started.physical_attempt_id
                            )
                        })?;
                    if attempt.is_terminal() {
                        return Ok(false);
                    }
                    if attempt.started_event_id != start_event_id
                        || attempt.last_causation_event_id != causation_id
                    {
                        bail!("provider recovery physical-attempt causal chain drifted");
                    }
                    if attempt.entry.request_material_fingerprint
                        != entry.request_material_fingerprint
                    {
                        bail!("provider recovery physical-attempt fingerprint drifted");
                    }
                    if attempt.causal_output_or_side_effect_event_ids
                        != entry.durable_output_event_ids
                    {
                        bail!("provider recovery physical-attempt output coverage drifted");
                    }
                    Ok(true)
                },
            )?;
            appended += usize::from(appended_event.is_some());
        }
        Ok(appended)
    }
}

pub(super) async fn append_direct_record_and_sync(
    store: JsonlSessionStore,
    session_id: SessionId,
    event_type: DurableEventType,
    payload: serde_json::Value,
    record_id: String,
    event_id: EventId,
    correlation_id: Option<EventId>,
    causation_id: Option<EventId>,
    guard: PhysicalAttemptAppendGuard,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let mut record = DurableAuditRecord::new(
            event_type,
            payload,
            record_id.clone(),
            correlation_id.clone(),
        )?
        .with_event_id(event_id.clone())?;
        if let Some(causation_id) = causation_id.clone() {
            record = record.with_causation_id(causation_id)?;
        }
        let reconciliation = record.reconciliation_expectation(session_id.clone())?;
        let mut expected =
            DurableAppendRecordExpectation::new(event_type, record_id.clone(), correlation_id)?
                .with_event_id(event_id)?;
        if let Some(causation_id) = causation_id {
            expected = expected.with_causation_id(causation_id)?;
        }
        let expectation =
            DurableAppendExpectation::new(session_id, record_id.clone(), vec![expected])?;
        let batch = DurableAuditBatch::new(record_id.clone(), vec![record])?;
        match store.append_audit_batch_if(batch, |records| guard.validate(records)) {
            Ok(Some(receipt)) => {
                DurableAuditWriter::validate_and_consume(&store, receipt, expectation)
                    .map(|_| ())
                    .map_err(anyhow::Error::from)
            }
            Ok(None) => bail!("provider physical-attempt durable append was not attempted"),
            Err(append_error) => match store.reconcile_durable_event(&reconciliation) {
                DurableEventReconciliation::ExactPresent(_) => Ok(()),
                DurableEventReconciliation::ConfirmedAbsent => Err(append_error
                    .context("provider physical-attempt durable append is confirmed absent")),
                DurableEventReconciliation::Conflict { reason } => Err(append_error.context(
                    format!("provider physical-attempt durable append conflicts: {reason}"),
                )),
                DurableEventReconciliation::Indeterminate { reason } => Err(append_error.context(
                    format!("provider physical-attempt durable append is indeterminate: {reason}"),
                )),
            },
        }
    })
    .await
    .context("provider physical-attempt durable append task failed")?
}

#[derive(Clone)]
pub(super) enum PhysicalAttemptAppendGuard {
    Start {
        physical_attempt_id: ProviderPhysicalAttemptId,
    },
    Output {
        physical_attempt_id: ProviderPhysicalAttemptId,
        start_event_id: EventId,
        causation_event_id: EventId,
    },
    Terminal {
        entry: ProviderPhysicalAttemptTerminalEntry,
        start_event_id: EventId,
        causation_event_id: EventId,
    },
}

impl PhysicalAttemptAppendGuard {
    pub(super) fn validate(&self, records: &[SessionStreamRecord]) -> Result<bool> {
        let projection = ProviderPhysicalAttemptProjection::from_records(records)?;
        match self {
            Self::Start {
                physical_attempt_id,
            } => {
                if projection.attempt(physical_attempt_id).is_some() {
                    bail!("provider physical attempt {physical_attempt_id} already exists");
                }
                Ok(true)
            }
            Self::Output {
                physical_attempt_id,
                start_event_id,
                causation_event_id,
            } => {
                let attempt = projection.attempt(physical_attempt_id).with_context(|| {
                    format!(
                        "provider output references missing physical attempt {physical_attempt_id}"
                    )
                })?;
                if attempt.is_terminal() {
                    bail!("provider output cannot be appended after its physical terminal");
                }
                if attempt.started_event_id != *start_event_id {
                    bail!("provider output start identity does not match physical attempt");
                }
                if attempt.last_causation_event_id != *causation_event_id {
                    bail!("provider output causation does not match physical attempt chain");
                }
                Ok(true)
            }
            Self::Terminal {
                entry,
                start_event_id,
                causation_event_id,
            } => {
                let attempt = projection
                    .attempt(&entry.physical_attempt_id)
                    .with_context(|| {
                        format!(
                            "provider physical-attempt terminal references missing attempt {}",
                            entry.physical_attempt_id
                        )
                    })?;
                if attempt.is_terminal() {
                    bail!(
                        "provider physical attempt {} already has a terminal event",
                        entry.physical_attempt_id
                    );
                }
                if attempt.started_event_id != *start_event_id
                    || attempt.last_causation_event_id != *causation_event_id
                {
                    bail!("provider physical-attempt terminal causal chain drifted");
                }
                if attempt.entry.request_material_fingerprint != entry.request_material_fingerprint
                {
                    bail!("provider physical-attempt terminal fingerprint drifted");
                }
                let expected_references = entry
                    .durable_output_event_ids
                    .iter()
                    .chain(entry.durable_side_effect_event_ids.iter())
                    .cloned()
                    .collect::<Vec<_>>();
                if attempt.causal_output_or_side_effect_event_ids != expected_references {
                    bail!("provider physical-attempt terminal output coverage drifted");
                }
                Ok(true)
            }
        }
    }
}

fn decode_attempt_payload<T>(event: &StoredEvent) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(event.payload.clone()).with_context(|| {
        format!(
            "failed to decode {} provider physical-attempt payload",
            event.event_type
        )
    })
}

fn validate_identity(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        bail!("{field} must be non-empty, bounded, and control-free");
    }
    Ok(())
}

fn validate_label(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("{field} must be non-empty, bounded, and control-free");
    }
    Ok(())
}

fn validate_reference_ids(field: &str, ids: &[EventId], max_count: usize) -> Result<()> {
    if ids.len() > max_count {
        bail!("{field} exceed maximum count {max_count}");
    }
    let mut unique = std::collections::BTreeSet::new();
    for id in ids {
        validate_identity(field, id)?;
        if !unique.insert(id) {
            bail!("{field} contain duplicate event ids");
        }
    }
    Ok(())
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
#[path = "tests/provider_attempt_tests.rs"]
mod tests;
