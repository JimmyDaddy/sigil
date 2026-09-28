use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use agent_client_protocol::{Client, ConnectionTo, schema::v1 as acp};
use anyhow::{Result, anyhow, ensure};
use sigil_application::LiveRunUpdateKind;
use sigil_kernel::{PublicRunEvent, PublicRunEventKind};
use sigil_runtime::application_run::{
    ApplicationRunEventHandler, RuntimeLivePreviewReader, RuntimeLivePreviewSource,
};

/// ACP chunks are append-only; replacement previews stay local until a verified suffix exists.
struct TextProjection {
    attempt_id: Option<String>,
    retired_attempts: BTreeSet<String>,
    text: String,
    reasoning: String,
    message_id: String,
    reasoning_message_id: String,
    text_complete: bool,
    reasoning_complete: bool,
    last_message: Option<String>,
}

impl Default for TextProjection {
    fn default() -> Self {
        Self {
            attempt_id: None,
            retired_attempts: BTreeSet::new(),
            text: String::new(),
            reasoning: String::new(),
            message_id: uuid::Uuid::new_v4().to_string(),
            reasoning_message_id: uuid::Uuid::new_v4().to_string(),
            text_complete: false,
            reasoning_complete: false,
            last_message: None,
        }
    }
}

impl TextProjection {
    fn reset_message(&mut self, reasoning: bool) {
        if reasoning {
            self.reasoning.clear();
            self.reasoning_message_id = uuid::Uuid::new_v4().to_string();
            self.reasoning_complete = false;
        } else {
            self.text.clear();
            self.message_id = uuid::Uuid::new_v4().to_string();
            self.text_complete = false;
        }
    }

    fn admit_attempt(&mut self, attempt_id: Option<String>, reasoning: bool) -> bool {
        if attempt_id
            .as_ref()
            .is_some_and(|id| self.retired_attempts.contains(id))
        {
            return false;
        }
        if self.attempt_id != attempt_id {
            if let Some(previous) = self.attempt_id.take() {
                self.retired_attempts.insert(previous);
            }
            self.attempt_id = attempt_id;
            self.reset_message(false);
            self.reset_message(true);
        }
        !(if reasoning {
            self.reasoning_complete
        } else {
            self.text_complete
        })
    }

    fn discard(&mut self) {
        if let Some(attempt_id) = self.attempt_id.as_ref() {
            self.retired_attempts.insert(attempt_id.clone());
        }
        self.reset_message(false);
        self.reset_message(true);
    }

    fn suffix<'a>(sent: &mut String, snapshot: &'a str) -> Option<&'a str> {
        let suffix = snapshot.strip_prefix(sent.as_str())?;
        if suffix.is_empty() {
            return None;
        }
        *sent = snapshot.to_owned();
        Some(suffix)
    }
}

pub(super) struct EventProjection {
    client: ConnectionTo<Client>,
    session_id: String,
    scope_id: String,
    run_id: String,
    preview: Option<RuntimeLivePreviewReader>,
    text: TextProjection,
}

impl EventProjection {
    pub(super) fn new(
        client: ConnectionTo<Client>,
        session_id: String,
        scope_id: String,
        run_id: String,
    ) -> Self {
        Self {
            client,
            session_id,
            scope_id,
            run_id,
            preview: None,
            text: TextProjection::default(),
        }
    }

    fn send(&self, update: acp::SessionUpdate) -> Result<()> {
        self.client
            .send_notification(acp::SessionNotification::new(
                self.session_id.clone(),
                update,
            ))
            .map_err(|error| anyhow!(error.to_string()))
    }

    fn chunk(&self, text: &str, reasoning: bool) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let message_id = if reasoning {
            &self.text.reasoning_message_id
        } else {
            &self.text.message_id
        };
        let chunk = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(text)))
            .message_id(acp::MessageId::new(message_id.clone()));
        self.send(if reasoning {
            acp::SessionUpdate::AgentThoughtChunk(chunk)
        } else {
            acp::SessionUpdate::AgentMessageChunk(chunk)
        })
    }

    pub(super) fn poll_preview(&mut self) -> Result<()> {
        let updates = match &mut self.preview {
            Some(reader) => reader.poll_updates()?,
            None => return Ok(()),
        };
        for update in updates {
            self.apply_preview(update)?;
        }
        Ok(())
    }

    fn apply_preview(&mut self, update: sigil_application::LiveRunUpdate) -> Result<()> {
        ensure!(
            update.session_id == self.scope_id && update.run_id == self.run_id,
            "ACP preview belongs to a different run"
        );
        if update.truncated {
            return Ok(());
        }
        let reasoning = match update.kind {
            LiveRunUpdateKind::Text => false,
            LiveRunUpdateKind::Reasoning => true,
            _ => return Ok(()),
        };
        if !self.text.admit_attempt(update.attempt_id, reasoning) {
            return Ok(());
        }
        let sent = if reasoning {
            &mut self.text.reasoning
        } else {
            &mut self.text.text
        };
        if let Some(suffix) = TextProjection::suffix(sent, update.preview.as_str()) {
            self.chunk(suffix, reasoning)?;
        }
        Ok(())
    }

    fn notice(&self, text: &str) -> Result<()> {
        self.send(acp::SessionUpdate::AgentMessageChunk(
            acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(text)))
                .message_id(acp::MessageId::new(uuid::Uuid::new_v4().to_string())),
        ))
    }

    fn finish_message(&mut self, content: &str, reasoning: bool) -> Result<()> {
        let sent = if reasoning {
            &mut self.text.reasoning
        } else {
            &mut self.text.text
        };
        let suffix = TextProjection::suffix(sent, content).map(str::to_owned);
        if let Some(suffix) = suffix {
            self.chunk(&suffix, reasoning)?;
        } else if sent != content {
            // ACP has no retraction operation. A replacement gets a new message identity;
            // never append it to the provisional text of a different provider attempt.
            self.text.reset_message(reasoning);
            self.chunk(content, reasoning)?;
            if reasoning {
                self.text.reasoning = content.to_owned();
            } else {
                self.text.text = content.to_owned();
            }
        }
        if reasoning {
            self.text.reasoning_complete = true;
        } else {
            self.text.text_complete = true;
        }
        Ok(())
    }

    fn event(&mut self, event: PublicRunEvent) -> Result<()> {
        ensure!(
            event.session_id == self.scope_id && event.run_id == self.run_id,
            "ACP event belongs to a different run"
        );
        match event.event {
            PublicRunEventKind::AssistantMessage { message } => {
                let reasoning = message.assistant_kind
                    == Some(sigil_kernel::AssistantMessageKind::ReasoningTrace);
                if let Some(content) = message.content {
                    self.finish_message(&content, reasoning)?;
                    if !reasoning {
                        self.text.last_message = Some(content);
                    }
                }
            }
            PublicRunEventKind::RunFinished { final_text } => {
                if self.text.last_message.as_deref() != Some(&final_text) {
                    self.finish_message(&final_text, false)?;
                }
            }
            PublicRunEventKind::ProviderTurnPartialOutputDiscarded { output } => {
                self.text.discard();
                if output.text_discarded || output.reasoning_discarded {
                    self.notice("The previous partial response was discarded.")?;
                }
            }
            PublicRunEventKind::ToolCallStarted { call } => {
                self.send(acp::SessionUpdate::ToolCall(
                    acp::ToolCall::new(call.id, call.name).status(acp::ToolCallStatus::Pending),
                ))?;
            }
            PublicRunEventKind::ToolCallCompleted { call } => {
                self.send(acp::SessionUpdate::ToolCallUpdate(
                    acp::ToolCallUpdate::new(
                        call.id,
                        acp::ToolCallUpdateFields::new()
                            .status(acp::ToolCallStatus::InProgress)
                            .raw_input(
                                serde_json::from_str::<serde_json::Value>(&call.args_json).ok(),
                            ),
                    ),
                ))?;
            }
            PublicRunEventKind::ToolResult { result } => {
                let status = if result.is_error() {
                    acp::ToolCallStatus::Failed
                } else {
                    acp::ToolCallStatus::Completed
                };
                self.send(acp::SessionUpdate::ToolCallUpdate(
                    acp::ToolCallUpdate::new(
                        result.call_id,
                        acp::ToolCallUpdateFields::new()
                            .status(status)
                            .content(vec![acp::ToolCallContent::Content(acp::Content::new(
                                acp::ContentBlock::Text(acp::TextContent::new(result.content)),
                            ))]),
                    ),
                ))?;
            }
            PublicRunEventKind::Notice { message } => self.notice(&message)?,
            PublicRunEventKind::RunFailed { error } => self.notice(&error)?,
            PublicRunEventKind::RunBlocked { reason }
            | PublicRunEventKind::RunPaused { reason }
            | PublicRunEventKind::RunInterrupted { reason } => self.notice(&reason)?,
            _ => {}
        }
        Ok(())
    }
}

pub(super) struct EventHandler(pub(super) Arc<Mutex<EventProjection>>);

impl ApplicationRunEventHandler for EventHandler {
    fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| anyhow!("ACP event projection unavailable"))?
            .event(event)
    }

    fn bind_live_preview_source(&mut self, source: RuntimeLivePreviewSource) -> Result<()> {
        let mut projection = self
            .0
            .lock()
            .map_err(|_| anyhow!("ACP event projection unavailable"))?;
        ensure!(
            source.session_id() == projection.scope_id && source.run_id() == projection.run_id,
            "ACP preview source belongs to a different run"
        );
        projection.preview = Some(source.reader());
        Ok(())
    }

    fn public_event_adapter_id(&self) -> &'static str {
        "acp"
    }
}

#[cfg(test)]
#[path = "tests/events_tests.rs"]
mod tests;
