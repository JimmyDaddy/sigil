use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::*;
use crate::{EventId, SessionId, projection_apply_decision};

/// Schema version for the provider physical-attempt direct payloads.
pub const PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION: u16 = 3;

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

/// Allocates one provider physical-attempt identity before its durable send barrier is written.
///
/// Continuation coordinators use this to bind their own append-only lifecycle to the exact
/// provider attempt that will later cross the send barrier. Allocation alone authorizes no I/O.
#[must_use]
pub fn new_provider_physical_attempt_id() -> ProviderPhysicalAttemptId {
    format!("provider-attempt-{}", Uuid::new_v4())
}

/// Maximum assistant text retained in memory from one semantic-compaction model request.
pub const MAX_SEMANTIC_COMPACTION_GENERATION_BYTES: usize =
    super::compaction_sidecar::MAX_SEMANTIC_COMPACTION_OUTPUT_BYTES;

/// Process-local result of one completed semantic-compaction provider request.
///
/// The raw text is never persisted as a conversation message. Only the validated portable
/// checkpoint may later become durable; usage is recorded separately under the physical attempt.
#[derive(Debug, Clone)]
pub struct SemanticCompactionGeneration {
    pub output_text: String,
    pub usage: Option<crate::UsageStats>,
}

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
    /// Present for all newly emitted attempts. `None` remains readable for pre-envelope V3 logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_envelope: Option<crate::ProviderRequestEnvelopeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_layout_proof: Option<crate::CacheLayoutProofV1>,
    /// V2 semantic proof excluding process-local hosted authorization identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_cache_layout_proof_v2: Option<crate::CacheLayoutProofV2>,
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
        let cache_layout = self
            .cache_layout_proof
            .as_ref()
            .context("provider physical-attempt cache layout proof is missing")?;
        cache_layout.validate()?;
        if let Some(semantic) = self.semantic_cache_layout_proof_v2.as_ref() {
            semantic.validate()?;
        }
        if let Some(request_envelope) = &self.request_envelope {
            request_envelope.validate()?;
            if request_envelope.provider_name != self.provider_name
                || request_envelope.model_name != self.model_name
                || request_envelope.process_local_material_fingerprint
                    != self.request_material_fingerprint
            {
                bail!("provider physical-attempt request envelope identity is inconsistent");
            }
            if request_envelope.cache_layout_hash != cache_layout.layout_hash {
                bail!("provider physical-attempt request envelope cache layout is inconsistent");
            }
        }
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
    /// An exact adapter-proven pre-generation rejection or pre-dispatch transport failure.
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

    /// Returns every physical attempt in durable start order for diagnostics and audit views.
    #[must_use]
    pub fn attempts(&self) -> Vec<&ProviderPhysicalAttemptState> {
        let mut attempts = self.attempts.values().collect::<Vec<_>>();
        attempts.sort_by_key(|attempt| attempt.started_stream_sequence);
        attempts
    }

    /// Returns every attempt with one durable logical-run correlation id.
    ///
    /// Results are ordered by durable start sequence. Callers that accept a bounded safe-connect
    /// retry chain should use [`Self::effective_attempt_for_logical_run_id`] instead of guessing
    /// which physical attempt consumed a queued input.
    #[must_use]
    pub fn attempts_for_logical_run_id(
        &self,
        logical_run_id: &str,
    ) -> Vec<&ProviderPhysicalAttemptState> {
        let mut attempts = self
            .attempts
            .values()
            .filter(|attempt| attempt.entry.logical_run_id == logical_run_id)
            .collect::<Vec<_>>();
        attempts.sort_by_key(|attempt| attempt.started_stream_sequence);
        attempts
    }

    /// Resolves the effective physical attempt for one logical run.
    ///
    /// Multiple attempts are accepted only when every predecessor is an exact-material,
    /// durably-terminal connect failure proven to have occurred before dispatch. The final
    /// attempt remains authoritative even when it is unfinished or terminal with another outcome.
    ///
    /// # Errors
    ///
    /// Returns an error when the chain changes provider/request identity, overlaps attempts, or
    /// contains a predecessor that was not a confirmed safe-connect retry.
    pub fn effective_attempt_for_logical_run_id(
        &self,
        logical_run_id: &str,
    ) -> Result<Option<&ProviderPhysicalAttemptState>> {
        let attempts = self.attempts_for_logical_run_id(logical_run_id);
        let Some((effective, predecessors)) = attempts.split_last() else {
            return Ok(None);
        };
        let first = attempts[0];
        for attempt in &attempts[1..] {
            if attempt.entry.purpose != first.entry.purpose
                || attempt.entry.request_material_fingerprint
                    != first.entry.request_material_fingerprint
                || attempt.entry.provider_name != first.entry.provider_name
                || attempt.entry.model_name != first.entry.model_name
            {
                bail!("provider physical-attempt retry chain changed frozen request identity");
            }
        }
        for pair in attempts.windows(2) {
            let [prior, next] = pair else {
                unreachable!("physical-attempt windows always have two members");
            };
            let Some(prior_terminal_sequence) = prior.terminal_stream_sequence else {
                bail!("provider physical-attempt retry predecessor has no durable terminal");
            };
            if prior_terminal_sequence >= next.started_stream_sequence {
                bail!("provider physical-attempt retry chain overlaps durable lifecycles");
            }
        }
        for predecessor in predecessors {
            let terminal = predecessor
                .terminal
                .as_ref()
                .context("provider physical-attempt retry predecessor has no terminal")?;
            if terminal.outcome != ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption
                || terminal.rejection
                    != Some(crate::ProviderRequestRejection::ConnectFailedBeforeDispatch)
                || !terminal.durable_output_event_ids.is_empty()
                || !terminal.durable_side_effect_event_ids.is_empty()
            {
                bail!(
                    "provider physical-attempt retry predecessor was not a confirmed pre-dispatch connect failure"
                );
            }
        }
        Ok(Some(effective))
    }

    /// Returns every physical attempt that was sent but has no durable terminal.
    #[must_use]
    pub fn unfinished_attempts(&self) -> Vec<&ProviderPhysicalAttemptState> {
        self.attempts
            .values()
            .filter(|attempt| !attempt.is_terminal())
            .collect()
    }

    pub(super) fn latest_cache_layout_proof_for_internal_use(
        &self,
    ) -> Option<&crate::CacheLayoutProofV1> {
        self.attempts
            .values()
            .filter_map(|attempt| {
                attempt
                    .entry
                    .cache_layout_proof
                    .as_ref()
                    .map(|proof| (attempt.started_stream_sequence, proof))
            })
            .max_by_key(|(sequence, _)| *sequence)
            .map(|(_, proof)| proof)
    }

    pub(super) fn latest_semantic_cache_layout_proof_v2_for_internal_use(
        &self,
    ) -> Option<&crate::CacheLayoutProofV2> {
        self.attempts
            .values()
            .filter_map(|attempt| {
                attempt
                    .entry
                    .semantic_cache_layout_proof_v2
                    .as_ref()
                    .map(|proof| (attempt.started_stream_sequence, proof))
            })
            .max_by_key(|(sequence, _)| *sequence)
            .map(|(_, proof)| proof)
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
        if entry.request_envelope.as_ref().is_some_and(|envelope| {
            envelope
                .source_frontier
                .as_ref()
                .is_some_and(|frontier| frontier.session_id != event.session_id)
        }) {
            bail!("provider request envelope source frontier belongs to a different session");
        }
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
    InMemory {
        semantic_cache_layout_proof_v2: crate::CacheLayoutProofV2,
    },
    Durable(DurableProviderPhysicalAttemptAudit),
}

pub(crate) struct DurableProviderPhysicalAttemptAudit {
    physical_attempt_id: ProviderPhysicalAttemptId,
    request_material_fingerprint: String,
    start_event_id: EventId,
    last_causation_event_id: EventId,
    durable_output_event_ids: Vec<EventId>,
    semantic_cache_layout_proof_v2: crate::CacheLayoutProofV2,
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
        Self::start_with_purpose_and_id(
            session,
            logical_run_id,
            frozen_request,
            ProviderPhysicalAttemptPurpose::ConversationGeneration,
            None,
        )
        .await
    }

    pub(crate) async fn start_with_id(
        session: &Session,
        logical_run_id: &str,
        frozen_request: &crate::FrozenProviderRequestMaterial,
        physical_attempt_id: &str,
    ) -> Result<Self> {
        Self::start_with_purpose_and_id(
            session,
            logical_run_id,
            frozen_request,
            ProviderPhysicalAttemptPurpose::ConversationGeneration,
            Some(physical_attempt_id),
        )
        .await
    }

    async fn start_with_purpose(
        session: &Session,
        logical_run_id: &str,
        frozen_request: &crate::FrozenProviderRequestMaterial,
        purpose: ProviderPhysicalAttemptPurpose,
    ) -> Result<Self> {
        Self::start_with_purpose_and_id(session, logical_run_id, frozen_request, purpose, None)
            .await
    }

    async fn start_with_purpose_and_id(
        session: &Session,
        logical_run_id: &str,
        frozen_request: &crate::FrozenProviderRequestMaterial,
        purpose: ProviderPhysicalAttemptPurpose,
        requested_physical_attempt_id: Option<&str>,
    ) -> Result<Self> {
        if let Some(physical_attempt_id) = requested_physical_attempt_id {
            validate_identity("provider physical attempt id", physical_attempt_id)?;
        }
        let Some(store) = session.durable_store() else {
            return Ok(Self::InMemory {
                semantic_cache_layout_proof_v2: frozen_request
                    .semantic_cache_layout_proof_v2(None)?,
            });
        };
        let request = frozen_request.request();
        let (prior_cache_layout, prior_semantic_cache_layout_v2) = {
            let store = store.clone();
            tokio::task::spawn_blocking(move || {
                let records = store.read_event_records_writer()?;
                let projection = ProviderPhysicalAttemptProjection::from_records(&records)?;
                Ok::<_, anyhow::Error>((
                    projection
                        .latest_cache_layout_proof_for_internal_use()
                        .cloned(),
                    projection
                        .latest_semantic_cache_layout_proof_v2_for_internal_use()
                        .cloned(),
                ))
            })
            .await
            .context("provider cache layout projection task failed")??
        };
        let cache_layout_proof =
            Some(frozen_request.cache_layout_proof(prior_cache_layout.as_ref())?);
        let semantic_cache_layout_proof_v2 = Some(
            frozen_request
                .semantic_cache_layout_proof_v2(prior_semantic_cache_layout_v2.as_ref())?,
        );
        let source_projection = session.active_projection_snapshot()?;
        let source_frontier = source_projection
            .as_ref()
            .map(|snapshot| crate::ProviderRequestSourceFrontierV1::from(snapshot.frontier()));
        let context_epoch_id = source_projection
            .as_ref()
            .map(|snapshot| snapshot.tool_output_pressure().active_epoch_id.clone())
            .unwrap_or_else(|| "context-epoch:root".to_owned());
        let durable_previous_response_handle =
            session.latest_response_handle(&request.provider_name);
        let durable_continuation_states = session.continuation_states(&request.provider_name);
        let request_envelope = frozen_request.request_envelope_with_durable_runtime_state(
            cache_layout_proof
                .as_ref()
                .expect("provider attempt cache layout is always materialized"),
            source_frontier,
            context_epoch_id,
            durable_previous_response_handle.as_ref(),
            &durable_continuation_states,
        )?;
        let physical_attempt_id = requested_physical_attempt_id
            .map(str::to_owned)
            .unwrap_or_else(new_provider_physical_attempt_id);
        let start_event_id = Uuid::new_v4().to_string();
        let entry = ProviderPhysicalAttemptStartedEntry {
            schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
            physical_attempt_id: physical_attempt_id.clone(),
            logical_run_id: logical_run_id.to_owned(),
            purpose,
            request_material_fingerprint: frozen_request.fingerprint().to_owned(),
            provider_name: request.provider_name.clone(),
            model_name: request.model_name.clone(),
            request_envelope: Some(request_envelope),
            cache_layout_proof,
            semantic_cache_layout_proof_v2,
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
            semantic_cache_layout_proof_v2: entry
                .semantic_cache_layout_proof_v2
                .expect("new physical attempts always carry a semantic cache layout proof"),
            terminal_recorded: false,
        }))
    }

    #[must_use]
    pub(crate) fn cache_layout_mutation(&self) -> crate::CacheLayoutMutationKind {
        match self {
            Self::InMemory {
                semantic_cache_layout_proof_v2,
            } => semantic_cache_layout_proof_v2.mutation_from_previous.kind,
            Self::Durable(audit) => {
                audit
                    .semantic_cache_layout_proof_v2
                    .mutation_from_previous
                    .kind
            }
        }
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

/// Executes one bounded, no-side-effect semantic-compaction model request.
///
/// Client tool schemas may remain in the frozen request to preserve the provider cache prefix,
/// but tool chunks are rejected and are never executed. Hosted tools must be absent because they
/// can execute remotely. Response handles and continuation states from this internal request are
/// deliberately ignored.
///
/// # Errors
///
/// Returns an error when request identity is inconsistent, durable attempt barriers fail, the
/// provider emits a tool/hosted/background action, output is empty or oversized, or the stream
/// terminates unsuccessfully.
pub async fn generate_semantic_compaction(
    provider: &dyn crate::Provider,
    session: &mut Session,
    logical_run_id: &str,
    frozen_request: crate::FrozenProviderRequestMaterial,
) -> Result<SemanticCompactionGeneration> {
    if frozen_request.session_scope_id() != session.session_scope_id() {
        bail!("semantic compaction request belongs to a different session scope");
    }
    let request = frozen_request.request();
    if request.provider_name != session.provider_name()
        || request.model_name != session.model_name()
        || provider.name() != session.provider_name()
    {
        bail!("semantic compaction request route does not match the durable session");
    }
    if request.background {
        bail!("semantic compaction request cannot run in background mode");
    }
    if !request.hosted_tools.is_empty() {
        bail!("semantic compaction request cannot expose hosted tools");
    }

    let request = request.clone();
    let pricing_snapshot = provider.usage_pricing_snapshot(&request.model_name);
    let mut physical_attempt = ProviderPhysicalAttemptAudit::start_with_purpose(
        session,
        logical_run_id,
        &frozen_request,
        ProviderPhysicalAttemptPurpose::SemanticCompaction,
    )
    .await?;
    let mut generation_observed = false;
    let result = async {
        let mut stream = provider.stream(request).await?;
        let mut output_text = String::new();
        let mut latest_usage = None;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("semantic compaction provider stream failed")?;
            match chunk {
                crate::ProviderChunk::TextDelta(delta) => {
                    generation_observed = true;
                    if output_text.len().saturating_add(delta.len())
                        > MAX_SEMANTIC_COMPACTION_GENERATION_BYTES
                    {
                        bail!("semantic compaction output exceeds its bounded size");
                    }
                    output_text.push_str(&delta);
                }
                crate::ProviderChunk::ReasoningDelta(_)
                | crate::ProviderChunk::ReasoningSummaryDelta(_) => {
                    generation_observed = true;
                }
                crate::ProviderChunk::Usage(mut usage) => {
                    generation_observed |= usage.completion_tokens > 0;
                    let mutation = physical_attempt.cache_layout_mutation();
                    usage
                        .cache_usage
                        .get_or_insert(crate::CacheUsageV1 {
                            schema_version: crate::CacheUsageV1::SCHEMA_VERSION,
                            read: None,
                            write: None,
                            uncached: None,
                            local_layout_mutation: None,
                            provider_miss_without_local_mutation: false,
                        })
                        .observe_local_layout(mutation);
                    if let Some(cache_usage) = &usage.cache_usage {
                        cache_usage.validate_for_prompt_tokens(usage.prompt_tokens)?;
                    }
                    if let Some(snapshot) = &pricing_snapshot {
                        usage = snapshot.apply_to_usage(usage)?;
                    }
                    session.stats_mut().apply_semantic_compaction_usage(&usage);
                    physical_attempt
                        .append_output_control(
                            session,
                            ControlEntry::SemanticCompactionUsageSnapshot(usage.clone()),
                        )
                        .await?;
                    latest_usage = Some(usage);
                }
                crate::ProviderChunk::ResponseHandle(_)
                | crate::ProviderChunk::ReasoningArtifact(_)
                | crate::ProviderChunk::ContinuationState(_) => {
                    generation_observed = true;
                }
                crate::ProviderChunk::Done => break,
                crate::ProviderChunk::ToolCallStart { .. }
                | crate::ProviderChunk::ToolCallArgsDelta { .. }
                | crate::ProviderChunk::ToolCallComplete(_)
                | crate::ProviderChunk::ToolCallStreamError(_) => {
                    generation_observed = true;
                    bail!("semantic compaction provider attempted a client tool call");
                }
                crate::ProviderChunk::HostedToolStarted { .. }
                | crate::ProviderChunk::HostedEvidence { .. }
                | crate::ProviderChunk::HostedToolFailed { .. }
                | crate::ProviderChunk::HostedRequestUsage { .. } => {
                    generation_observed = true;
                    bail!("semantic compaction provider attempted a hosted tool action");
                }
                crate::ProviderChunk::BackgroundTaskAccepted(_)
                | crate::ProviderChunk::BackgroundTaskStatus(_) => {
                    generation_observed = true;
                    bail!("semantic compaction provider attempted a background action");
                }
            }
        }
        if output_text.trim().is_empty() {
            bail!("semantic compaction provider returned no summary JSON");
        }
        Ok(SemanticCompactionGeneration {
            output_text,
            usage: latest_usage,
        })
    }
    .await;

    let rejection = (!generation_observed && !physical_attempt.has_durable_output_or_side_effect())
        .then(|| {
            result
                .as_ref()
                .err()
                .and_then(|error| provider.classify_pre_generation_rejection(error))
        })
        .flatten();
    let outcome = match &result {
        Ok(_) => ProviderPhysicalAttemptOutcome::Completed,
        Err(_) if rejection.is_some() => {
            ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption
        }
        Err(_) if physical_attempt.has_durable_output_or_side_effect() => {
            ProviderPhysicalAttemptOutcome::FailedAfterOutputOrSideEffect
        }
        Err(_) if generation_observed => {
            ProviderPhysicalAttemptOutcome::ProtocolRejectedAfterOutput
        }
        Err(_) => ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain,
    };
    let terminal = physical_attempt.finish(session, outcome, rejection).await;
    match (result, terminal) {
        (Ok(generation), Ok(())) => Ok(generation),
        (Ok(_), Err(error)) => {
            Err(error.context("semantic compaction physical-attempt terminal append failed"))
        }
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(terminal_error)) => Err(error.context(format!(
            "semantic compaction failed and its physical terminal also failed: {terminal_error:#}"
        ))),
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
