use std::sync::{Arc, Mutex, mpsc};

use anyhow::{Result, anyhow};
use sigil_kernel::{
    ControlEntry, EventHandler, ProviderOutputPublicationIntentV1, PublicEventOutboxEntryV1,
    RunEvent, Session, SessionLogEntry, SessionPublicEventProjectionV1, StoredEvent,
};

use super::protocol::WorkerMessage;
use super::worker_loop::RunTaskPayload;

#[derive(Clone)]
pub(super) struct ChannelEventHandler {
    sender: mpsc::Sender<WorkerMessage>,
    recorder: Option<Arc<Mutex<sigil_runtime::ApplicationRunEventRecorder>>>,
    public_run_identity: Option<(String, String, std::path::PathBuf)>,
    pending_handoff_terminal: bool,
}

struct NativeEventDelivery<'a>(&'a ChannelEventHandler);

impl EventHandler for NativeEventDelivery<'_> {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        self.0.forward(event)
    }
}

impl ChannelEventHandler {
    pub(super) fn new(sender: mpsc::Sender<WorkerMessage>) -> Self {
        Self {
            sender,
            recorder: None,
            public_run_identity: None,
            pending_handoff_terminal: false,
        }
    }

    pub(super) fn start_public_run(
        &mut self,
        session: &Session,
        run_id: &str,
        prompt: &str,
    ) -> Result<()> {
        if let Some((session_id, bound_run_id, store_path)) = &self.public_run_identity {
            if session_id == session.session_scope_id()
                && bound_run_id == run_id
                && session.store_path() == Some(store_path.as_path())
            {
                return Ok(());
            }
            anyhow::bail!("native event handler is already bound to another public run");
        }
        let recorder = sigil_runtime::ApplicationRunEventRecorder::start(session, run_id, prompt)?;
        self.sender
            .send(WorkerMessage::LivePreviewSource {
                source: recorder.live_preview_source(),
            })
            .map_err(|error| anyhow!("failed to attach live preview source: {error}"))?;
        self.sender
            .send(WorkerMessage::LivePreviewDurableFrontier {
                session_id: session.session_scope_id().to_owned(),
                run_id: run_id.to_owned(),
                sequence: recorder.public_sequence()?,
            })
            .map_err(|error| anyhow!("failed to attach live preview frontier: {error}"))?;
        self.recorder = Some(Arc::new(Mutex::new(recorder)));
        self.public_run_identity = Some((
            session.session_scope_id().to_owned(),
            run_id.to_owned(),
            session
                .store_path()
                .ok_or_else(|| anyhow!("public run requires a durable session"))?
                .to_path_buf(),
        ));
        self.pending_handoff_terminal = true;
        Ok(())
    }

    pub(super) fn finish_public_run(
        &mut self,
        result: &Result<sigil_kernel::AgentRunOutput>,
    ) -> Result<()> {
        if let Some(recorder) = &self.recorder {
            let recorder = recorder
                .lock()
                .map_err(|_| anyhow!("application event recorder is unavailable"))?;
            match result {
                Ok(output) => {
                    self.pending_handoff_terminal = !matches!(
                        output.disposition,
                        sigil_kernel::AgentRunDisposition::FinalAnswer
                            | sigil_kernel::AgentRunDisposition::AwaitingUserInput(_)
                            | sigil_kernel::AgentRunDisposition::Interrupted
                            | sigil_kernel::AgentRunDisposition::Blocked
                    );
                    recorder.finish_output(output)?;
                }
                Err(error) => recorder.finish_error(error)?,
            }
        }
        Ok(())
    }

    /// Handoffs remain open while the Task/Plan owner is running. Its final payload closes the
    /// same root public stream; ordinary kernel terminals are already committed and preserved.
    pub(super) fn finish_public_payload(
        &mut self,
        session: &mut Session,
        payload: &mut RunTaskPayload,
    ) -> Result<()> {
        if !self.pending_handoff_terminal {
            return Ok(());
        }
        if let RunTaskPayload::Chat {
            result: Ok(result), ..
        }
        | RunTaskPayload::Agent {
            result: Ok(result), ..
        } = payload
            && result.final_message_id.is_none()
        {
            // A host-generated Plan result is a real parent answer too. Persist it through
            // the same source/outbox boundary before the lifecycle terminal can reference it.
            let mut message = sigil_kernel::ModelMessage::assistant(
                Some(sigil_kernel::safe_persistence_text(&result.final_text)),
                Vec::new(),
            );
            message.assistant_kind = Some(sigil_kernel::AssistantMessageKind::FinalAnswer);
            let message_id = message.id.clone();
            self.commit_session_publications(
                session,
                vec![SessionLogEntry::Assistant(message.clone())],
                vec![SessionPublicEventProjectionV1::assistant_message(
                    0, message,
                )],
            )?;
            result.final_message_id = Some(message_id);
        }
        let Some(recorder) = &self.recorder else {
            return Ok(());
        };
        let recorder = recorder
            .lock()
            .map_err(|_| anyhow!("application event recorder is unavailable"))?;
        match payload {
            RunTaskPayload::Chat { result, .. } | RunTaskPayload::Agent { result, .. } => {
                match result {
                    Ok(result) => recorder.finish_output(&sigil_kernel::AgentRunOutput {
                        disposition: sigil_kernel::AgentRunDisposition::FinalAnswer,
                        result: result.clone(),
                        outcome: sigil_kernel::AgentRunOutcome::default(),
                    }),
                    Err(error) => recorder.finish_error(&anyhow!(error.clone())),
                }
            }
            RunTaskPayload::AwaitingUserInput { request } => {
                recorder.finish_output(&sigil_kernel::AgentRunOutput {
                    disposition: sigil_kernel::AgentRunDisposition::AwaitingUserInput(
                        request.clone(),
                    ),
                    result: sigil_kernel::AgentRunResult {
                        final_text: String::new(),
                        tool_calls: 0,
                        final_message_id: None,
                    },
                    outcome: sigil_kernel::AgentRunOutcome::default(),
                })
            }
            RunTaskPayload::Task {
                task_id, result, ..
            } => match result {
                Ok(status) => recorder.finish_task(
                    session,
                    &sigil_kernel::TaskId::new(task_id.clone()).map_err(anyhow::Error::msg)?,
                    *status,
                ),
                Err(error) => recorder.finish_error(&anyhow!(error.clone())),
            },
            RunTaskPayload::PlanReviewBlocked { reason, paused } => {
                recorder.finish_blocked(*paused, reason)
            }
            RunTaskPayload::PlanReviewCancelled => {
                recorder.finish_cancelled(false, "plan review cancelled")
            }
            RunTaskPayload::PlanReviewInterrupted { reason } => {
                recorder.finish_cancelled(true, reason)
            }
        }
    }

    fn forward(&self, event: RunEvent) -> Result<()> {
        if matches!(
            event,
            RunEvent::TextDelta(_)
                | RunEvent::ReasoningDelta(_)
                | RunEvent::ToolCallArgsDelta { .. }
                | RunEvent::ToolProgress(_)
        ) {
            return Ok(());
        }
        self.sender
            .send(WorkerMessage::Event(Box::new(event)))
            .map_err(|error| anyhow!("failed to forward run event: {error}"))?;
        if let Some(recorder) = &self.recorder {
            let recorder = recorder
                .lock()
                .map_err(|_| anyhow!("application event recorder is unavailable"))?;
            let source = recorder.live_preview_source();
            self.sender
                .send(WorkerMessage::LivePreviewDurableFrontier {
                    session_id: source.session_id().to_owned(),
                    run_id: source.run_id().to_owned(),
                    sequence: recorder.public_sequence()?,
                })
                .map_err(|error| anyhow!("failed to forward live preview frontier: {error}"))?;
        }
        Ok(())
    }
}

impl EventHandler for ChannelEventHandler {
    fn begin_live_attempt(&mut self, physical_attempt_id: &str) -> Result<()> {
        if let Some(recorder) = &self.recorder {
            recorder
                .lock()
                .map_err(|_| anyhow!("application event recorder is unavailable"))?
                .begin_live_attempt(physical_attempt_id)?;
        }
        Ok(())
    }

    fn handle(&mut self, event: RunEvent) -> Result<()> {
        if let Some(recorder) = &self.recorder {
            recorder
                .lock()
                .map_err(|_| anyhow!("application event recorder is unavailable"))?
                .handle(event.clone())?;
        }
        self.forward(event)
    }

    fn commit_controls(
        &mut self,
        session: &mut Session,
        controls: Vec<ControlEntry>,
    ) -> Result<Vec<StoredEvent>> {
        let events = if let Some(recorder) = &self.recorder {
            recorder
                .lock()
                .map_err(|_| anyhow!("application event recorder is unavailable"))?
                .commit_controls(session, controls.clone())?
        } else {
            session.append_controls_with_events(controls.clone())?
        };
        for control in controls {
            self.forward(RunEvent::Control(control))?;
        }
        Ok(events)
    }

    fn commit_session_publications(
        &mut self,
        session: &mut Session,
        entries: Vec<SessionLogEntry>,
        publications: Vec<SessionPublicEventProjectionV1>,
    ) -> Result<Vec<StoredEvent>> {
        if let Some(recorder) = &self.recorder {
            let events = recorder
                .lock()
                .map_err(|_| anyhow!("application event recorder is unavailable"))?
                .commit_session_publications(session, entries.clone(), publications.clone())?;
            NativeEventDelivery(self)
                .handle_committed_session_publications(entries, publications)?;
            Ok(events)
        } else {
            NativeEventDelivery(self).commit_session_publications(session, entries, publications)
        }
    }

    fn prepare_provider_output_publication(
        &mut self,
        session: &Session,
        control: &ControlEntry,
    ) -> Result<Option<ProviderOutputPublicationIntentV1>> {
        match &self.recorder {
            Some(recorder) => recorder
                .lock()
                .map_err(|_| anyhow!("application event recorder is unavailable"))?
                .prepare_provider_output_publication(session, control),
            None => Ok(None),
        }
    }

    fn complete_provider_output_publication(
        &mut self,
        intent: Option<ProviderOutputPublicationIntentV1>,
        committed: Vec<PublicEventOutboxEntryV1>,
        event: RunEvent,
    ) -> Result<()> {
        let public = intent
            .as_ref()
            .is_none_or(ProviderOutputPublicationIntentV1::is_public);
        if let Some(recorder) = &self.recorder {
            recorder
                .lock()
                .map_err(|_| anyhow!("application event recorder is unavailable"))?
                .complete_provider_output_publication(intent, committed, event.clone())?;
        } else if intent.is_some() || !committed.is_empty() {
            anyhow::bail!("provider output publication requires an owning event bridge");
        }
        if public {
            self.forward(event)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/live_preview_bridge_tests.rs"]
mod tests;
