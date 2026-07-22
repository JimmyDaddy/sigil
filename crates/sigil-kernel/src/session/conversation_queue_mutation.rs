use anyhow::{Context, Result};

use super::*;
use crate::{
    ConversationInputPromotedEntry, ConversationInputStatus, ConversationInputTerminalCommand,
    ConversationInputTerminalExpectation, ConversationQueueMutationCommand,
    ConversationQueueMutationReceipt, ConversationQueueRevision, DurableEventType,
    event::canonical_json_content_hash,
};

impl JsonlSessionStore {
    /// Atomically validates and appends one ordinary conversation queue mutation.
    ///
    /// The expected revision check and append both run while holding the JSONL single-writer
    /// lease. A stale or invalid command leaves the stream byte-for-byte unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error when the command is stale or invalid, the durable queue cannot be
    /// reconstructed, or the append cannot be persisted and synchronized.
    pub fn append_conversation_queue_mutation(
        &self,
        command: ConversationQueueMutationCommand,
    ) -> Result<ConversationQueueMutationReceipt> {
        let control = command.mutation.control_entry();
        let entry = SessionLogEntry::Control(control);
        let event_type = session_entry_event_type(&entry);
        let payload = serde_json::json!({ "session_log_entry": entry });
        let session_id = conversation_queue_mutation_session_id(self)?;
        let identity_digest = canonical_json_content_hash(
            &serde_json::to_value(&command)
                .context("failed to encode conversation queue mutation command")?,
        )?;
        let event_id = stable_event_uuid(
            "sigil-conversation-queue-mutation",
            &format!("{session_id}:{identity_digest}"),
        );

        let event = self.append_event_if_with_identity(
            event_type,
            payload,
            event_id.clone(),
            Some(event_id),
            None,
            |records| {
                let projection = ConversationQueueDurableProjection::from_records(records)?;
                projection.validate_mutation(&command)?;
                Ok(true)
            },
        )?;
        let event = event.context("conversation queue mutation append was not attempted")?;
        let revision = ConversationQueueRevision {
            stream_sequence: event.stream_sequence,
            event_id: event.event_id.clone(),
        };
        Ok(ConversationQueueMutationReceipt { revision, event })
    }

    /// Appends one terminal queue status only while its preparation or promoted-run predicate is
    /// still current under the JSONL single-writer lease.
    ///
    /// Predicate drift is an expected race outcome: it returns `Ok(None)` and leaves the stream
    /// byte-for-byte unchanged. A queued expectation binds the exact queue revision, item status,
    /// safe prompt hash, and FIFO ownership. A promoted expectation binds the dispatching item to
    /// its one durable promotion and logical dispatch run id.
    ///
    /// # Errors
    ///
    /// Returns an error when the command is malformed or non-terminal, the durable queue cannot be
    /// reconstructed, a promotion payload is malformed, or the accepted append cannot be
    /// persisted and synchronized.
    pub fn append_conversation_input_terminal_if_current(
        &self,
        command: ConversationInputTerminalCommand,
    ) -> Result<Option<StoredEvent>> {
        command.validate_shape()?;
        let entry = SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(
            command.terminal.clone(),
        ));
        let event_type = session_entry_event_type(&entry);
        let payload = serde_json::json!({ "session_log_entry": entry });
        let session_id = conversation_queue_mutation_session_id(self)?;
        let identity_digest = canonical_json_content_hash(
            &serde_json::to_value(&command)
                .context("failed to encode conversation input terminal command")?,
        )?;
        let event_id = stable_event_uuid(
            "sigil-conversation-input-terminal",
            &format!("{session_id}:{identity_digest}"),
        );

        self.append_event_if_with_identity(
            event_type,
            payload,
            event_id.clone(),
            Some(event_id),
            None,
            |records| conversation_input_terminal_predicate(records, &command),
        )
    }
}

fn conversation_queue_mutation_session_id(store: &JsonlSessionStore) -> Result<String> {
    let records = store.read_event_records_writer()?;
    Ok(stream_session_id(&records).unwrap_or_else(|| session_id_for_path(store.path())))
}

fn conversation_input_terminal_predicate(
    records: &[SessionStreamRecord],
    command: &ConversationInputTerminalCommand,
) -> Result<bool> {
    let projection = ConversationQueueDurableProjection::from_records(records)?;
    let item = projection
        .queue
        .items
        .iter()
        .find(|item| item.queued.queue_id == command.terminal.queue_id);
    match &command.expectation {
        ConversationInputTerminalExpectation::Queued {
            expected_queue_revision,
            queue_id,
            expected_prompt_hash,
        } => Ok(projection.current_revision() == *expected_queue_revision
            && projection.queue.next_dispatchable.as_ref() == Some(queue_id)
            && item.is_some_and(|item| {
                item.status == ConversationInputStatus::Queued
                    && item.queued.prompt_hash == *expected_prompt_hash
            })),
        ConversationInputTerminalExpectation::Promoted {
            queue_id,
            dispatch_run_id,
            expected_frontier,
        } => {
            if !records
                .last()
                .is_some_and(|record| expected_frontier.matches_record(record))
            {
                return Ok(false);
            }
            if !item.is_some_and(|item| item.status == ConversationInputStatus::Dispatching) {
                return Ok(false);
            }
            let promotions = records
                .iter()
                .filter(|record| {
                    record.stored_event().event_kind()
                        == Some(DurableEventType::ConversationInputPromoted)
                })
                .map(|record| {
                    serde_json::from_value::<ConversationInputPromotedEntry>(
                        record.stored_event().payload.clone(),
                    )
                    .context("failed to decode conversation input promoted payload")
                })
                .filter(|entry| {
                    entry
                        .as_ref()
                        .is_ok_and(|promotion| promotion.queue_id == *queue_id)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(matches!(
                promotions.as_slice(),
                [promotion] if promotion.dispatch_run_id == *dispatch_run_id
            ))
        }
    }
}
