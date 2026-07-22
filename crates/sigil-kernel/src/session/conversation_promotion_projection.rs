use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use super::*;
use crate::{ConversationInputQueueId, ConversationInputStatus, EventId};

/// Decodes one durable event into its canonical transcript entry.
///
/// `ConversationInputPromoted` is the only durable event for its safe user message. Queue/state
/// projections retain the control entry, while transcript/fork/export consumers use this helper
/// to observe the embedded user message at the promotion event's original order and identity.
///
/// # Errors
///
/// Returns an error when the durable envelope or promotion payload is malformed.
pub fn conversation_transcript_entry_from_record(
    record: &SessionStreamRecord,
) -> Result<Option<SessionLogEntry>> {
    Ok(
        match session_entry_from_stored_event(record.stored_event())? {
            Some(SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promotion))) => {
                Some(SessionLogEntry::User(promotion.durable_user_message))
            }
            entry => entry,
        },
    )
}

pub(crate) fn delivered_conversation_queue_ids_from_entries(
    entries: &[SessionLogEntry],
) -> BTreeSet<ConversationInputQueueId> {
    let mut statuses = BTreeMap::new();
    for entry in entries {
        match entry {
            SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promotion)) => {
                statuses
                    .entry(promotion.queue_id.clone())
                    .or_insert(ConversationInputStatus::Dispatching);
            }
            SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status)) => {
                statuses.insert(status.queue_id.clone(), status.status);
            }
            _ => {}
        }
    }
    statuses
        .into_iter()
        .filter_map(|(queue_id, status)| {
            (status == ConversationInputStatus::Delivered).then_some(queue_id)
        })
        .collect()
}

pub(crate) fn provider_visible_conversation_promotion_event_ids(
    records: &[SessionStreamRecord],
) -> Result<BTreeSet<EventId>> {
    ConversationQueueDurableProjection::from_records(records)?;
    let entries = records
        .iter()
        .map(|record| session_entry_from_stored_event(record.stored_event()))
        .collect::<Result<Vec<_>>>()?;
    let flat_entries = entries.iter().flatten().cloned().collect::<Vec<_>>();
    let delivered = delivered_conversation_queue_ids_from_entries(&flat_entries);
    let mut visible = BTreeSet::new();
    for (record, entry) in records.iter().zip(entries) {
        if let Some(SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promotion))) =
            entry
            && delivered.contains(&promotion.queue_id)
        {
            visible.insert(record.event_id().to_owned());
        }
    }
    Ok(visible)
}
