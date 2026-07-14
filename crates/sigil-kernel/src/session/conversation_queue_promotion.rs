use anyhow::{Context, Result};

use super::*;
use crate::{
    ConversationInputPromotedEntry, ConversationQueueDurableProjection, EventId, stable_event_uuid,
};

impl JsonlSessionStore {
    /// Appends the single critical event that atomically promotes a queued main-thread input.
    ///
    /// The compare-and-swap predicate runs under the JSONL single-writer lease. A stale queue
    /// revision, pause, edit, cancellation, reordering, or previous promotion leaves the stream
    /// unchanged and returns an error.
    pub fn append_conversation_input_promoted(
        &self,
        entry: ConversationInputPromotedEntry,
    ) -> Result<StoredEvent> {
        let session_id = conversation_queue_session_id(self)?;
        entry.validate_for_session(&session_id)?;
        let event_id = conversation_input_promotion_event_id(&session_id, &entry);
        let payload = serde_json::to_value(&entry)
            .context("failed to encode conversation input promoted event")?;
        let event = self.append_event_if_with_identity(
            DurableEventType::ConversationInputPromoted,
            payload,
            event_id.clone(),
            Some(event_id),
            None,
            |records| {
                let projection = ConversationQueueDurableProjection::from_records(records)?;
                projection.validate_promotion(&entry)?;
                Ok(true)
            },
        )?;
        event.context("conversation input promotion append was not attempted")
    }
}

fn conversation_queue_session_id(store: &JsonlSessionStore) -> Result<String> {
    let records = store.read_event_records_writer()?;
    Ok(stream_session_id(&records).unwrap_or_else(|| session_id_for_path(store.path())))
}

fn conversation_input_promotion_event_id(
    session_id: &str,
    entry: &ConversationInputPromotedEntry,
) -> EventId {
    stable_event_uuid(
        "sigil-conversation-input-promotion",
        &format!(
            "{session_id}:{}:{}:{}",
            entry.queue_id.as_str(),
            entry.expected_queue_revision.event_id,
            entry.dispatch_run_id
        ),
    )
}
