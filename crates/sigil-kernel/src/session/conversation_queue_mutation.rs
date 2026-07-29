use anyhow::{Context, Result};

use super::*;
use crate::{
    ConversationInputTerminalCommand, ConversationInputTerminalExpectation,
    ConversationQueueMutation, ConversationQueueMutationCommand, ConversationQueueMutationReceipt,
    ConversationQueueRevision, event::canonical_json_content_hash,
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
        let session_id = self
            .active_projection_snapshot()?
            .frontier()
            .session_id()
            .to_owned();
        let enqueue_uses_stable_identity =
            matches!(&command.mutation, ConversationQueueMutation::Enqueue { .. });
        let identity_digest = match &command.mutation {
            // Queue ids are append-only identities. A stable enqueue event id preserves global
            // uniqueness after terminal tombstones leave the bounded active projection.
            ConversationQueueMutation::Enqueue { entry } => {
                format!("enqueue:{}", entry.queue_id.as_str())
            }
            _ => canonical_json_content_hash(
                &serde_json::to_value(&command)
                    .context("failed to encode conversation queue mutation command")?,
            )?,
        };
        let event_id = stable_event_uuid(
            "sigil-conversation-queue-mutation",
            &format!("{session_id}:{identity_digest}"),
        );

        let event = self.append_event_if_active_projection(
            event_type,
            payload,
            event_id.clone(),
            Some(event_id),
            None,
            None,
            |projection| {
                projection.queue().validate_mutation(&command)?;
                Ok(true)
            },
        );
        let event = match event {
            Ok(event) => event,
            Err(error)
                if enqueue_uses_stable_identity
                    && format!("{error:#}").contains("already exists in the durable stream") =>
            {
                anyhow::bail!("conversation queue mutation queue id already exists")
            }
            Err(error) => return Err(error),
        };
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
        let session_id = self
            .active_projection_snapshot()?
            .frontier()
            .session_id()
            .to_owned();
        let identity_digest = canonical_json_content_hash(
            &serde_json::to_value(&command)
                .context("failed to encode conversation input terminal command")?,
        )?;
        let event_id = stable_event_uuid(
            "sigil-conversation-input-terminal",
            &format!("{session_id}:{identity_digest}"),
        );

        self.append_event_if_active_projection(
            event_type,
            payload,
            event_id.clone(),
            Some(event_id),
            None,
            None,
            |projection| {
                let promoted_frontier_matches = match &command.expectation {
                    ConversationInputTerminalExpectation::Queued { .. } => true,
                    ConversationInputTerminalExpectation::Promoted {
                        expected_frontier, ..
                    } => projection.frontier.cursor.as_ref().is_some_and(|cursor| {
                        cursor.last_applied_stream_sequence == expected_frontier.stream_sequence
                            && cursor.last_applied_event_id == expected_frontier.event_id
                            && cursor.last_applied_record_checksum
                                == expected_frontier.record_checksum
                    }),
                };
                projection
                    .queue()
                    .validate_terminal(&command, promoted_frontier_matches)
            },
        )
    }
}
