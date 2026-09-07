use anyhow::Result;

use super::*;
use crate::{
    ConversationInputPromotedEntry, ConversationInputQueueId, ConversationQueueRevision,
    MessageRole, ModelMessage, conversation_promotion_capability_digest,
};

fn message(id: &str, role: MessageRole, content: Option<&str>) -> ModelMessage {
    let mut message = ModelMessage::new(role, content.map(str::to_owned));
    message.id = id.to_owned();
    message
}

fn promotion(message: ModelMessage) -> Result<ConversationInputPromotedEntry> {
    Ok(ConversationInputPromotedEntry {
        queue_id: ConversationInputQueueId::new("source-user-message")?,
        expected_queue_revision: ConversationQueueRevision::initial(),
        prompt_hash: "source-user-message-hash".to_owned(),
        exact_prompt_required: false,
        durable_user_message: message,
        capability_descriptors: Vec::new(),
        capability_digest: conversation_promotion_capability_digest(&[])?,
        dispatch_run_id: "source-user-message-run".to_owned(),
        promoted_at_ms: 1,
    })
}

#[test]
fn source_user_message_returns_direct_user_entry() {
    let session = Session::from_entries(
        "provider",
        "model",
        vec![SessionLogEntry::User(message(
            "direct-user",
            MessageRole::User,
            Some("direct objective"),
        ))],
    );

    assert_eq!(
        session
            .source_user_message("direct-user")
            .and_then(|message| message.content.as_deref()),
        Some("direct objective")
    );
}

#[test]
fn source_user_message_returns_promoted_durable_user_entry() -> Result<()> {
    let session = Session::from_entries(
        "provider",
        "model",
        vec![SessionLogEntry::Control(
            ControlEntry::ConversationInputPromoted(promotion(message(
                "promoted-user",
                MessageRole::User,
                Some("promoted objective"),
            ))?),
        )],
    );

    assert_eq!(
        session
            .source_user_message("promoted-user")
            .and_then(|message| message.content.as_deref()),
        Some("promoted objective")
    );
    Ok(())
}

#[test]
fn source_user_message_ignores_missing_and_non_user_entries() {
    let session = Session::from_entries(
        "provider",
        "model",
        vec![SessionLogEntry::Assistant(message(
            "not-a-user",
            MessageRole::Assistant,
            Some("assistant text"),
        ))],
    );

    assert!(session.source_user_message("missing").is_none());
    assert!(session.source_user_message("not-a-user").is_none());
}

#[test]
fn source_user_message_preserves_absent_content() {
    let session = Session::from_entries(
        "provider",
        "model",
        vec![SessionLogEntry::User(message(
            "empty-user",
            MessageRole::User,
            None,
        ))],
    );

    assert!(
        session
            .source_user_message("empty-user")
            .is_some_and(|message| message.content.is_none())
    );
}

#[test]
fn source_user_message_returns_the_first_matching_durable_user_source() -> Result<()> {
    let session = Session::from_entries(
        "provider",
        "model",
        vec![
            SessionLogEntry::Assistant(message(
                "shared-id",
                MessageRole::Assistant,
                Some("not a source"),
            )),
            SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promotion(
                message("shared-id", MessageRole::User, Some("first source")),
            )?)),
            SessionLogEntry::User(message(
                "shared-id",
                MessageRole::User,
                Some("later source"),
            )),
        ],
    );

    assert_eq!(
        session
            .source_user_message("shared-id")
            .and_then(|message| message.content.as_deref()),
        Some("first source")
    );
    Ok(())
}
