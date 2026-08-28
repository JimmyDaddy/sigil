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
    deliveries: BTreeMap<String, BTreeSet<String>>,
}

impl PublicEventOutboxProjectionV1 {
    /// Rebuilds the outbox from critical durable records. A corrupt entry is a projection
    /// degradation, never evidence that the underlying application run failed.
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let mut projection = Self::default();
        for record in records {
            projection.apply_record(record)?;
        }
        Ok(projection)
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
            Some(DurableEventType::PublicEventOutbox) => self.apply_outbox(decode(event)?)?,
            Some(DurableEventType::PublicEventDeliveryReceipt) => {
                self.apply_delivery(decode(event)?)?
            }
            Some(_) | None => {}
        }
        self.cursor = Some(record.projection_cursor(PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION));
        Ok(())
    }

    pub fn apply_outbox(&mut self, entry: PublicEventOutboxEntryV1) -> Result<()> {
        validate_outbox_entry(&entry)?;
        if self
            .entries
            .insert(entry.public_event_id.clone(), entry)
            .is_some()
        {
            bail!("public event outbox entry was recorded more than once");
        }
        Ok(())
    }

    pub fn apply_delivery(&mut self, receipt: PublicEventDeliveryReceiptV1) -> Result<()> {
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
        let mut entries: Vec<_> = self.entries.values().collect();
        entries.sort_by(|left, right| {
            left.sequence
                .cmp(&right.sequence)
                .then_with(|| left.public_event_id.cmp(&right.public_event_id))
        });
        entries
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

    /// Appends one idempotent public outbox record before an adapter receives it.
    pub fn append_outbox(&self, entry: &PublicEventOutboxEntryV1) -> Result<bool> {
        validate_outbox_entry(entry)?;
        let entry = entry.clone();
        self.store.append_event_if(
            DurableEventType::PublicEventOutbox,
            EventClass::Critical,
            serde_json::to_value(&entry).context("failed to encode public outbox event")?,
            move |records| {
                let projection = PublicEventOutboxProjectionV1::from_records(records)?;
                match projection.entry(&entry.public_event_id) {
                    Some(existing) if outbox_entries_match(existing, &entry)? => Ok(false),
                    Some(_) => bail!("public event id is reused with conflicting payload"),
                    None => Ok(true),
                }
            },
        )
    }

    /// Appends the idempotent receipt after an adapter accepted the exact public event.
    pub fn append_delivery(&self, receipt: &PublicEventDeliveryReceiptV1) -> Result<bool> {
        validate_delivery_receipt(receipt)?;
        let receipt = receipt.clone();
        self.store.append_event_if(
            DurableEventType::PublicEventDeliveryReceipt,
            EventClass::Critical,
            serde_json::to_value(&receipt).context("failed to encode public delivery receipt")?,
            move |records| {
                let projection = PublicEventOutboxProjectionV1::from_records(records)?;
                if projection.entry(&receipt.public_event_id).is_none() {
                    bail!("public event delivery receipt has no outbox authority");
                }
                Ok(projection
                    .pending_for_adapter(&receipt.adapter)
                    .iter()
                    .any(|entry| entry.public_event_id == receipt.public_event_id))
            },
        )
    }
}

pub(crate) fn validate_outbox_entry(entry: &PublicEventOutboxEntryV1) -> Result<()> {
    if entry.schema_version != PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION
        || entry.public_event_id.trim().is_empty()
        || entry.domain_event_id.trim().is_empty()
        || entry.run_id.trim().is_empty()
        || entry.sequence == 0
        || !entry.payload_digest.starts_with("sha256:")
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

fn outbox_entries_match(
    existing: &PublicEventOutboxEntryV1,
    candidate: &PublicEventOutboxEntryV1,
) -> Result<bool> {
    Ok(existing.schema_version == candidate.schema_version
        && existing.public_event_id == candidate.public_event_id
        && existing.domain_event_id == candidate.domain_event_id
        && existing.run_id == candidate.run_id
        && existing.sequence == candidate.sequence
        && existing.payload_digest == candidate.payload_digest
        && serde_json::to_value(&existing.event)? == serde_json::to_value(&candidate.event)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: u64) -> crate::PublicRunEvent {
        crate::PublicRunEvent::new(
            "session-1".to_owned(),
            "run-1".to_owned(),
            sequence,
            crate::PublicRunEventKind::Notice {
                message: "safe".to_owned(),
            },
        )
    }

    fn entry(sequence: u64) -> PublicEventOutboxEntryV1 {
        let event = event(sequence);
        PublicEventOutboxEntryV1 {
            schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: format!("event-{sequence}"),
            domain_event_id: format!("domain-{sequence}"),
            run_id: event.run_id.clone(),
            sequence,
            payload_digest: crate::stable_event_hash(
                serde_json::to_vec(&event).expect("public event encodes"),
            ),
            event,
        }
    }

    #[test]
    fn outbox_projection_keeps_failed_delivery_pending_without_changing_domain_event() -> Result<()>
    {
        let entry = entry(1);
        let mut projection = PublicEventOutboxProjectionV1::default();
        projection.apply_outbox(entry.clone())?;
        assert_eq!(projection.pending_for_adapter("http").len(), 1);
        projection.apply_delivery(PublicEventDeliveryReceiptV1 {
            schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: entry.public_event_id.clone(),
            adapter: "http".to_owned(),
            delivered_at_unix_ms: 1,
        })?;
        assert!(projection.pending_for_adapter("http").is_empty());
        assert_eq!(projection.pending_for_adapter("desktop").len(), 1);
        Ok(())
    }

    #[test]
    fn outbox_projection_orders_state_events_independently_of_delivery() -> Result<()> {
        let first = entry(1);
        let second = entry(2);
        let mut projection = PublicEventOutboxProjectionV1::default();
        projection.apply_outbox(second)?;
        projection.apply_outbox(first.clone())?;
        projection.apply_delivery(PublicEventDeliveryReceiptV1 {
            schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: first.public_event_id,
            adapter: "tui".to_owned(),
            delivered_at_unix_ms: 1,
        })?;

        let events = projection.events_in_order();
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(events.len(), 2);
        assert!(projection.pending_for_adapter("tui").len() == 1);
        Ok(())
    }
}
