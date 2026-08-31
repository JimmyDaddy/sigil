use anyhow::Result;

use crate::{
    ConversationRunFinalizedEntryV1, ConversationRunTerminalStatusV1,
    PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION, PublicEventOutboxEntryV1, PublicRunEvent,
    PublicRunEventKind, stable_event_hash,
};

pub(crate) fn terminal_outbox(
    session_id: &str,
    terminal: &ConversationRunFinalizedEntryV1,
) -> Result<PublicEventOutboxEntryV1> {
    let summary = terminal.safe_summary().unwrap_or_default().to_owned();
    let kind = match terminal.status() {
        ConversationRunTerminalStatusV1::Succeeded => PublicRunEventKind::RunFinished {
            final_text: summary,
        },
        ConversationRunTerminalStatusV1::Failed => PublicRunEventKind::RunFailed { error: summary },
        ConversationRunTerminalStatusV1::Cancelled => PublicRunEventKind::RunCancelled,
        ConversationRunTerminalStatusV1::Interrupted => {
            PublicRunEventKind::RunInterrupted { reason: summary }
        }
        ConversationRunTerminalStatusV1::Paused => {
            PublicRunEventKind::RunPaused { reason: summary }
        }
        ConversationRunTerminalStatusV1::Blocked => {
            PublicRunEventKind::RunBlocked { reason: summary }
        }
        ConversationRunTerminalStatusV1::AwaitingUserInput => {
            PublicRunEventKind::RunAwaitingUserInput {
                request_id: "request-1".to_owned(),
                generation: 1,
                request_hash: stable_event_hash(b"request-1"),
            }
        }
    };
    let event = PublicRunEvent::new(session_id.to_owned(), terminal.run_id().to_owned(), 1, kind);
    Ok(PublicEventOutboxEntryV1 {
        schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: format!("test-public:{}", terminal.run_id()),
        domain_event_id: format!("test-domain:{}", terminal.run_id()),
        run_id: terminal.run_id().to_owned(),
        sequence: 1,
        payload_digest: stable_event_hash(serde_json::to_vec(&event)?),
        event,
    })
}
