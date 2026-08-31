use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::*;
use crate::projection_apply_decision;

/// Durable schema for public-event outbox records.
pub const PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION: u16 = 1;

/// Public event retained before it is dispatched to one product adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PublicEventOutboxEntryV1 {
    pub schema_version: u16,
    pub public_event_id: String,
    pub domain_event_id: String,
    pub run_id: String,
    pub sequence: u64,
    pub payload_digest: String,
    /// The already-bounded public DTO. Private transcript and tool arguments never enter it.
    pub event: crate::PublicRunEvent,
}

/// Idempotent adapter acknowledgement for one exact public event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PublicEventDeliveryReceiptV1 {
    pub schema_version: u16,
    pub public_event_id: String,
    pub adapter: String,
    pub delivered_at_unix_ms: u64,
}

/// Direct-event projection for replaying only unacknowledged public events.
#[derive(Debug, Clone, Default)]
pub struct PublicEventOutboxProjectionV1 {
    cursor: Option<ProjectionCursor>,
    entries: BTreeMap<String, PublicEventOutboxEntryV1>,
    ordered_ids: Vec<String>,
    deliveries: BTreeMap<String, BTreeSet<String>>,
    run_watermarks: BTreeMap<String, u64>,
}

impl PublicEventOutboxProjectionV1 {
    /// Rebuilds the outbox from critical durable records. A corrupt entry is a projection
    /// degradation, never evidence that the underlying application run failed.
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        crate::conversation_run::validate_conversation_run_lifecycle(records)?;
        let mut projection = Self::default();
        for record in records {
            projection.apply_record(record)?;
        }
        projection.validate_terminal_pairs(records)?;
        super::plan_review_terminal::validate_revision_pairs(records, &projection)?;
        Ok(projection)
    }

    fn validate_terminal_pairs(&self, records: &[SessionStreamRecord]) -> Result<()> {
        let mut terminals = BTreeMap::new();
        let mut envelopes = BTreeMap::new();
        for record in records {
            let event = record.stored_event();
            envelopes.insert(event.event_id.as_str(), event);
            if event.event_kind() == Some(DurableEventType::RunFinalized)
                && let Some(crate::ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(
                    terminal,
                )) = crate::conversation_run_lifecycle_record_from_stream(record)?
            {
                terminals.insert(event.event_id.as_str(), (event, terminal));
            }
        }
        let mut paired = BTreeSet::new();
        for entry in self
            .entries
            .values()
            .filter(|entry| is_terminal_event(&entry.event.event))
        {
            if envelopes
                .get(entry.domain_event_id.as_str())
                .is_some_and(|event| {
                    event.event_kind() == Some(DurableEventType::PlanReviewAttempt)
                })
            {
                // The revision domain validates its own exact attempt/draft/decision bundle.
                continue;
            }
            let (domain, terminal) = terminals
                .get(entry.domain_event_id.as_str())
                .context("public terminal has no exact conversation domain event")?;
            crate::conversation_run::validate_terminal_outbox(terminal, entry)?;
            let public = envelopes
                .get(entry.public_event_id.as_str())
                .context("public terminal has no exact outbox envelope")?;
            if public.event_kind() != Some(DurableEventType::PublicEventOutbox)
                || public.session_id != entry.event.session_id
                || domain.session_id != entry.event.session_id
                || domain.stream_sequence.checked_add(1) != Some(public.stream_sequence)
                || !paired.insert(entry.domain_event_id.as_str())
            {
                bail!("public terminal and domain do not form one exact durable pair");
            }
        }
        if terminals.keys().any(|id| !paired.contains(id)) {
            bail!("conversation terminal has no exact public outbox pair");
        }
        Ok(())
    }

    fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        let event = record.stored_event();
        if projection_apply_decision(self.cursor.as_ref(), event)?
            == ProjectionApplyDecision::IgnoreAlreadyApplied
        {
            return Ok(());
        }
        match decode_stored_event(event.clone())? {
            StoredEventDecode::Known(_) | StoredEventDecode::UnknownNonCritical(_) => {}
        }
        match event.event_kind() {
            Some(DurableEventType::PublicEventOutbox) => {
                let entry = decode(event)?;
                validate_outbox_envelope(event, &entry)?;
                self.apply_outbox(entry)?;
            }
            Some(DurableEventType::PublicEventDeliveryReceipt) => {
                self.apply_delivery(decode(event)?)?
            }
            Some(_) | None => {}
        }
        self.cursor = Some(record.projection_cursor(PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION));
        Ok(())
    }

    pub(crate) fn apply_outbox(&mut self, entry: PublicEventOutboxEntryV1) -> Result<()> {
        validate_outbox_entry(&entry)?;
        let public_event_id = entry.public_event_id.clone();
        if self.entries.contains_key(&public_event_id) {
            bail!("public event outbox entry was recorded more than once");
        }
        validate_sequence_advance(&mut self.run_watermarks, &entry)?;
        self.entries.insert(public_event_id.clone(), entry);
        self.ordered_ids.push(public_event_id);
        Ok(())
    }

    fn apply_delivery(&mut self, receipt: PublicEventDeliveryReceiptV1) -> Result<()> {
        validate_delivery_receipt(&receipt)?;
        if !self.entries.contains_key(&receipt.public_event_id) {
            bail!("public event delivery receipt has no outbox predecessor");
        }
        if !self
            .deliveries
            .entry(receipt.public_event_id)
            .or_default()
            .insert(receipt.adapter)
        {
            bail!("public event delivery receipt was recorded more than once for its adapter");
        }
        Ok(())
    }

    #[must_use]
    pub fn entry(&self, public_event_id: &str) -> Option<&PublicEventOutboxEntryV1> {
        self.entries.get(public_event_id)
    }

    #[must_use]
    pub fn pending_for_adapter(&self, adapter: &str) -> Vec<&PublicEventOutboxEntryV1> {
        self.entries
            .values()
            .filter(|entry| {
                !self
                    .deliveries
                    .get(&entry.public_event_id)
                    .is_some_and(|adapters| adapters.contains(adapter))
            })
            .collect()
    }

    /// Returns the durable public events in stream order for rebuilding a surface projection.
    /// Delivery receipts intentionally do not affect this view: an adapter receipt records
    /// transport progress, not whether the event is still part of the session's state history.
    #[must_use]
    pub fn events_in_order(&self) -> Vec<&PublicEventOutboxEntryV1> {
        self.ordered_ids
            .iter()
            .filter_map(|event_id| self.entries.get(event_id))
            .collect()
    }

    /// Returns the highest durable public-event sequence for `run_id`.
    #[must_use]
    pub fn durable_sequence(&self, run_id: &str) -> u64 {
        self.run_watermarks.get(run_id).copied().unwrap_or(0)
    }
}

/// Payload-free writer-side admission state for public-event appends.
///
/// The durable replay projection deliberately retains public DTOs for surface recovery. The
/// single writer only needs identities, digests, watermarks, and receipts to admit the next
/// append, so it keeps this separate index instead of retaining another copy of every event body.
#[derive(Debug, Default)]
pub(super) struct PublicEventOutboxAdmissionIndexV1 {
    cursor: Option<ProjectionCursor>,
    entries: BTreeMap<String, PublicEventOutboxIdentityV1>,
    run_watermarks: BTreeMap<String, u64>,
    deliveries: BTreeSet<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublicEventOutboxIdentityV1 {
    domain_event_id: String,
    run_id: String,
    sequence: u64,
    payload_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PublicEventOutboxAppendDecisionV1 {
    Append,
    AlreadyRecorded,
}

impl PublicEventOutboxAdmissionIndexV1 {
    pub(super) fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        // Keep canonical root/revision pair validation on the replay path. This index is only an
        // admission cache; it must never weaken the durable projection's corruption checks.
        PublicEventOutboxProjectionV1::from_records(records)?;
        let mut index = Self::default();
        for record in records {
            index.apply_record(record)?;
        }
        Ok(index)
    }

    pub(super) fn apply_stored_events(&mut self, events: &[StoredEvent]) -> Result<()> {
        for event in events {
            self.apply_record(&SessionStreamRecord::Stored(event.clone()))?;
        }
        Ok(())
    }

    pub(super) fn durable_sequence(&self, run_id: &str) -> u64 {
        self.run_watermarks.get(run_id).copied().unwrap_or(0)
    }

    pub(super) fn admit_outbox(
        &self,
        entry: &PublicEventOutboxEntryV1,
    ) -> Result<PublicEventOutboxAppendDecisionV1> {
        validate_outbox_entry(entry)?;
        if let Some(existing) = self.entries.get(&entry.public_event_id) {
            return if identity_matches_entry(existing, entry) {
                Ok(PublicEventOutboxAppendDecisionV1::AlreadyRecorded)
            } else {
                bail!("public event id is reused with conflicting payload")
            };
        }
        let watermark = self.durable_sequence(&entry.run_id);
        let expected = watermark
            .checked_add(1)
            .context("public event sequence exhausted")?;
        if entry.sequence != expected {
            bail!("public event sequence does not follow the durable run watermark");
        }
        Ok(PublicEventOutboxAppendDecisionV1::Append)
    }

    pub(super) fn admit_delivery(
        &self,
        receipt: &PublicEventDeliveryReceiptV1,
    ) -> Result<PublicEventOutboxAppendDecisionV1> {
        validate_delivery_receipt(receipt)?;
        if !self.entries.contains_key(&receipt.public_event_id) {
            bail!("public event delivery receipt has no outbox authority");
        }
        if self
            .deliveries
            .contains(&(receipt.public_event_id.clone(), receipt.adapter.clone()))
        {
            return Ok(PublicEventOutboxAppendDecisionV1::AlreadyRecorded);
        }
        Ok(PublicEventOutboxAppendDecisionV1::Append)
    }

    pub(super) fn validate_durable_appends(
        &self,
        entries: &[PublicEventOutboxEntryV1],
        receipts: &[PublicEventDeliveryReceiptV1],
    ) -> Result<()> {
        let mut pending_ids = BTreeSet::new();
        let mut pending_watermarks = BTreeMap::<String, u64>::new();
        for entry in entries {
            validate_outbox_entry(entry)?;
            if self.entries.contains_key(&entry.public_event_id)
                || !pending_ids.insert(entry.public_event_id.as_str())
            {
                bail!("public event outbox entry was recorded more than once");
            }
            let watermark = pending_watermarks
                .get(&entry.run_id)
                .copied()
                .or_else(|| self.run_watermarks.get(&entry.run_id).copied());
            validate_next_sequence(watermark.unwrap_or(0), entry.sequence)?;
            pending_watermarks.insert(entry.run_id.clone(), entry.sequence);
        }

        let mut pending_deliveries = BTreeSet::new();
        for receipt in receipts {
            validate_delivery_receipt(receipt)?;
            if !self.entries.contains_key(&receipt.public_event_id) {
                bail!("public event delivery receipt has no outbox predecessor");
            }
            let key = (receipt.public_event_id.as_str(), receipt.adapter.as_str());
            if self
                .deliveries
                .contains(&(receipt.public_event_id.clone(), receipt.adapter.clone()))
                || !pending_deliveries.insert(key)
            {
                bail!("public event delivery receipt was recorded more than once for its adapter");
            }
        }
        Ok(())
    }

    fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        let event = record.stored_event();
        if projection_apply_decision(self.cursor.as_ref(), event)?
            == ProjectionApplyDecision::IgnoreAlreadyApplied
        {
            return Ok(());
        }
        match event.event_kind() {
            Some(DurableEventType::PublicEventOutbox) => {
                let entry = decode(event)?;
                validate_outbox_envelope(event, &entry)?;
                self.apply_outbox(entry)?;
            }
            Some(DurableEventType::PublicEventDeliveryReceipt) => {
                self.apply_delivery(decode(event)?)?
            }
            Some(_) | None => {}
        }
        self.cursor = Some(record.projection_cursor(PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION));
        Ok(())
    }

    fn apply_outbox(&mut self, entry: PublicEventOutboxEntryV1) -> Result<()> {
        validate_outbox_entry(&entry)?;
        let public_event_id = entry.public_event_id.clone();
        if self.entries.contains_key(&public_event_id) {
            bail!("public event outbox entry was recorded more than once");
        }
        validate_sequence_advance(&mut self.run_watermarks, &entry)?;
        let identity = PublicEventOutboxIdentityV1::from(&entry);
        self.entries.insert(public_event_id, identity);
        Ok(())
    }

    fn apply_delivery(&mut self, receipt: PublicEventDeliveryReceiptV1) -> Result<()> {
        validate_delivery_receipt(&receipt)?;
        if !self.entries.contains_key(&receipt.public_event_id) {
            bail!("public event delivery receipt has no outbox predecessor");
        }
        if !self
            .deliveries
            .insert((receipt.public_event_id, receipt.adapter))
        {
            bail!("public event delivery receipt was recorded more than once for its adapter");
        }
        Ok(())
    }
}

impl From<&PublicEventOutboxEntryV1> for PublicEventOutboxIdentityV1 {
    fn from(entry: &PublicEventOutboxEntryV1) -> Self {
        Self {
            domain_event_id: entry.domain_event_id.clone(),
            run_id: entry.run_id.clone(),
            sequence: entry.sequence,
            payload_digest: entry.payload_digest.clone(),
        }
    }
}

/// Store-backed writer for the public outbox and adapter receipts.
#[derive(Debug, Clone)]
pub struct PublicEventOutboxRecorder {
    store: JsonlSessionStore,
}

impl PublicEventOutboxRecorder {
    #[must_use]
    pub fn new(store: JsonlSessionStore) -> Self {
        Self { store }
    }

    /// Appends one idempotent nonterminal public event before an adapter receives it.
    /// Terminal events must use their domain owner's crash-safe terminal bundle.
    pub fn append_outbox(&self, entry: &PublicEventOutboxEntryV1) -> Result<bool> {
        validate_outbox_entry(entry)?;
        if is_terminal_event(&entry.event.event) {
            bail!("public terminal requires the conversation terminal/outbox bundle");
        }
        self.store.append_public_event_outbox(entry)
    }

    /// Appends the idempotent receipt after an adapter accepted the exact public event.
    pub fn append_delivery(&self, receipt: &PublicEventDeliveryReceiptV1) -> Result<bool> {
        validate_delivery_receipt(receipt)?;
        self.store.append_public_event_delivery(receipt)
    }

    /// Returns the exact durable public-event frontier for `run_id`.
    pub fn durable_sequence(&self, run_id: &str) -> Result<u64> {
        if run_id.trim().is_empty() {
            bail!("public event run id is malformed");
        }
        self.store.public_event_outbox_durable_sequence(run_id)
    }
}

pub(super) fn is_terminal_event(event: &crate::PublicRunEventKind) -> bool {
    matches!(
        event,
        crate::PublicRunEventKind::RunFinished { .. }
            | crate::PublicRunEventKind::RunFailed { .. }
            | crate::PublicRunEventKind::RunCancelled
            | crate::PublicRunEventKind::RunInterrupted { .. }
            | crate::PublicRunEventKind::RunPaused { .. }
            | crate::PublicRunEventKind::RunBlocked { .. }
            | crate::PublicRunEventKind::RunAwaitingUserInput { .. }
    )
}

pub(crate) fn validate_outbox_entry(entry: &PublicEventOutboxEntryV1) -> Result<()> {
    if entry.schema_version != PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION
        || entry.public_event_id.trim().is_empty()
        || entry.domain_event_id.trim().is_empty()
        || entry.run_id.trim().is_empty()
        || entry.sequence == 0
        || !entry.payload_digest.starts_with("sha256:")
        || entry.event.schema_version != crate::PUBLIC_RUN_EVENT_SCHEMA_VERSION
        || entry.event.run_id != entry.run_id
        || entry.event.sequence != entry.sequence
        || entry.public_event_id.len() > 256
        || entry.domain_event_id.len() > 256
    {
        bail!("public event outbox entry is malformed");
    }
    let encoded =
        serde_json::to_vec(&entry.event).context("failed to encode public outbox payload")?;
    if entry.payload_digest != crate::stable_event_hash(&encoded) {
        bail!("public event outbox payload digest does not match its event");
    }
    if !is_terminal_event(&entry.event.event) && entry.domain_event_id != entry.public_event_id {
        bail!("nonterminal public event must self-reference its durable publication");
    }
    Ok(())
}

pub(crate) fn validate_delivery_receipt(receipt: &PublicEventDeliveryReceiptV1) -> Result<()> {
    if receipt.schema_version != PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION
        || receipt.public_event_id.trim().is_empty()
        || receipt.delivered_at_unix_ms == 0
        || receipt.adapter.is_empty()
        || receipt.adapter.len() > 96
        || !receipt
            .adapter
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        bail!("public event delivery receipt is malformed");
    }
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(event: &StoredEvent) -> Result<T> {
    serde_json::from_value(event.payload.clone()).context("failed to decode public outbox event")
}

fn validate_outbox_envelope(event: &StoredEvent, entry: &PublicEventOutboxEntryV1) -> Result<()> {
    if event.event_id != entry.public_event_id || event.session_id != entry.event.session_id {
        bail!("public outbox envelope does not match its exact public event identity");
    }
    Ok(())
}

fn identity_matches_entry(
    existing: &PublicEventOutboxIdentityV1,
    candidate: &PublicEventOutboxEntryV1,
) -> bool {
    existing.domain_event_id == candidate.domain_event_id
        && existing.run_id == candidate.run_id
        && existing.sequence == candidate.sequence
        && existing.payload_digest == candidate.payload_digest
}

fn validate_sequence_advance(
    watermarks: &mut BTreeMap<String, u64>,
    entry: &PublicEventOutboxEntryV1,
) -> Result<()> {
    validate_next_sequence(
        watermarks.get(&entry.run_id).copied().unwrap_or(0),
        entry.sequence,
    )?;
    watermarks.insert(entry.run_id.clone(), entry.sequence);
    Ok(())
}

fn validate_next_sequence(watermark: u64, sequence: u64) -> Result<()> {
    let next = watermark
        .checked_add(1)
        .context("public event sequence exhausted")?;
    if sequence != next {
        bail!("public event sequence is not the next durable run sequence");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/public_event_outbox_tests.rs"]
mod tests;
