//! Exact-record message hydration using the attachment's existing validated metadata index.

use sha2::{Digest, Sha256};
use sigil_application::message_content::{
    MessageContentError, MessageContentPage, MessageContentQuery,
};

use super::{read_model::RecordPosition, *};

#[derive(Debug, Clone)]
pub(super) struct MessageContentPosition {
    position: RecordPosition,
    message_id: String,
}

fn message_body<'a>(entry: &'a SessionLogEntry, event_id: &'a str) -> Option<(&'a str, &'a str)> {
    match entry {
        SessionLogEntry::User(message) | SessionLogEntry::Assistant(message) => message
            .content
            .as_deref()
            .or_else(|| (!message.image_attachments.is_empty()).then_some(""))
            .map(|text| (message.id.as_str(), text)),
        SessionLogEntry::Control(ControlEntry::Note { kind, data })
            if kind == "reasoning_trace" =>
        {
            data.get("text")
                .and_then(serde_json::Value::as_str)
                .map(|text| (event_id, text))
        }
        _ => None,
    }
}

pub(super) fn index_message_content(
    state: &mut ProjectionRecordCache,
    record: &SessionStreamRecord,
    position: &RecordPosition,
    entry: &SessionLogEntry,
) -> Result<(), ApplicationError> {
    if let Some((message_id, text)) = message_body(entry, record.event_id())
        && (!text.is_empty()
            || matches!(entry, SessionLogEntry::User(message) if !message.image_attachments.is_empty()))
    {
        let display_id = crate::conversation_display::stable_display_id(
            record.session_id(),
            record.event_id(),
            0,
        );
        if state
            .message_content
            .insert(
                display_id,
                MessageContentPosition {
                    position: position.clone(),
                    message_id: message_id.to_owned(),
                },
            )
            .is_some()
        {
            return Err(ApplicationError::CorruptProjection(
                "duplicate message content identity".into(),
            ));
        }
    }
    Ok(())
}

fn query_error(error: ApplicationError) -> MessageContentError {
    match error {
        ApplicationError::ScopeMismatch | ApplicationError::NotFound => {
            MessageContentError::NotFound
        }
        ApplicationError::ResetRequired => MessageContentError::Stale,
        ApplicationError::CorruptProjection(_) => MessageContentError::Corrupt,
        ApplicationError::InvalidRequest(_) => MessageContentError::InvalidQuery,
        _ => MessageContentError::Unavailable,
    }
}

impl RuntimeSessionProjectionOwner {
    /// Resolves a selected attachment from this session's immutable indexed message record.
    pub fn message_image_attachment(
        &self,
        expected_scope: &str,
        display_id: &str,
        attachment_id: &str,
        budget: &sigil_kernel::SessionReadBudget,
    ) -> Result<sigil_kernel::ImageAttachment, MessageContentError> {
        MessageContentQuery {
            display_id: display_id.to_owned(),
            offset: 0,
            limit: 4,
            content_version: None,
        }
        .validate()?;
        if attachment_id.is_empty() || attachment_id.len() > 128 {
            return Err(MessageContentError::InvalidQuery);
        }
        let mut state = self.state(budget).map_err(query_error)?;
        if state.session_id.as_deref() != Some(expected_scope) {
            return Err(MessageContentError::NotFound);
        }
        let selected = state
            .message_content
            .get(display_id)
            .cloned()
            .ok_or(MessageContentError::NotFound)?;
        if selected.position.end - selected.position.offset > 2 * 1024 * 1024 {
            return Err(MessageContentError::Corrupt);
        }
        let record = state
            .read_position(self, &selected.position, budget, true)
            .map_err(query_error)?;
        let entry = sigil_kernel::conversation_transcript_entry_from_record(&record)
            .map_err(|_| MessageContentError::Corrupt)?
            .ok_or(MessageContentError::Stale)?;
        let SessionLogEntry::User(message) = entry else {
            return Err(MessageContentError::NotFound);
        };
        if message.id != selected.message_id {
            return Err(MessageContentError::Stale);
        }
        let attachment = message
            .image_attachments
            .into_iter()
            .find(|image| image.attachment_id == attachment_id)
            .ok_or(MessageContentError::NotFound)?;
        attachment
            .validate()
            .map_err(|_| MessageContentError::Corrupt)?;
        Ok(attachment)
    }

    /// Hydrates one immutable message record, never a history prefix, into a UTF-8-safe page.
    pub fn message_content_page(
        &self,
        expected_session_scope_id: &str,
        query: &MessageContentQuery,
        budget: &sigil_kernel::SessionReadBudget,
    ) -> Result<MessageContentPage, MessageContentError> {
        query.validate()?;
        let mut state = self.state(budget).map_err(query_error)?;
        if state.session_id.as_deref() != Some(expected_session_scope_id) {
            return Err(MessageContentError::NotFound);
        }
        let selected = state
            .message_content
            .get(&query.display_id)
            .cloned()
            .ok_or(MessageContentError::NotFound)?;
        // The strict source reader has the same per-record cap. Keep it explicit at this seam.
        if selected.position.end - selected.position.offset > 2 * 1024 * 1024 {
            return Err(MessageContentError::Corrupt);
        }
        let mut digest = Sha256::new();
        digest.update(b"sigil-message-content-v1\0");
        digest.update(expected_session_scope_id.as_bytes());
        digest.update([0]);
        digest.update(query.display_id.as_bytes());
        digest.update([0]);
        digest.update(selected.position.checksum.as_bytes());
        let version = format!("{:x}", digest.finalize());
        if query
            .content_version
            .as_ref()
            .is_some_and(|expected| expected != &version)
        {
            return Err(MessageContentError::Stale);
        }
        let record = state
            .read_position(self, &selected.position, budget, true)
            .map_err(query_error)?;
        let entry = sigil_kernel::conversation_transcript_entry_from_record(&record)
            .map_err(|_| MessageContentError::Corrupt)?
            .ok_or(MessageContentError::Stale)?;
        let (message_id, body) =
            message_body(&entry, record.event_id()).ok_or(MessageContentError::Stale)?;
        if message_id != selected.message_id {
            return Err(MessageContentError::Stale);
        }
        budget
            .check()
            .map_err(|_| MessageContentError::Unavailable)?;
        // Redact before slicing so a secret cannot escape by straddling a page boundary.
        let body = sigil_kernel::safe_persistence_text(body);
        budget
            .check()
            .map_err(|_| MessageContentError::Unavailable)?;
        let start = usize::try_from(query.offset).map_err(|_| MessageContentError::InvalidQuery)?;
        if start > body.len() || !body.is_char_boundary(start) {
            return Err(MessageContentError::InvalidQuery);
        }
        let mut end = start.saturating_add(query.limit).min(body.len());
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        let text = body[start..end].to_owned();
        budget
            .check()
            .map_err(|_| MessageContentError::Unavailable)?;
        Ok(MessageContentPage {
            display_id: query.display_id.clone(),
            message_id: crate::application_run::safe_application_transcript_message_id(
                &selected.message_id,
            ),
            content_version: version,
            offset: query.offset,
            next_offset: (end < body.len()).then_some(end as u64),
            total_bytes: body.len() as u64,
            text,
        })
    }
}
