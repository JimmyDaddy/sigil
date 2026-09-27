use super::*;

/// Produces the canonical durable application stream for an adapter whose observer reads the
/// session projection asynchronously. Delivery acknowledgements belong to that observer;
/// appending an outbox entry here never acknowledges that a user surface has consumed it.
pub struct ApplicationRunEventRecorder {
    events: ApplicationRunEventSequence,
    task_events: PublicTaskEventProjector,
    lifecycle: ConversationRunLifecycleRecorder,
    publication_uncertain: bool,
}

struct DeferredApplicationDelivery;

impl ApplicationRunEventHandler for DeferredApplicationDelivery {
    fn handle_public_event(&mut self, _event: PublicRunEvent) -> Result<()> {
        bail!("a deferred application producer cannot acknowledge observer delivery")
    }
}

impl ApplicationRunEventRecorder {
    /// Returns the same bounded live source used by interactive public adapters. Deferred
    /// durable delivery does not suppress previews or create delivery receipts for them.
    pub fn live_preview_source(&self) -> RuntimeLivePreviewSource {
        self.events.live_preview.clone()
    }

    /// Returns the committed public watermark paired with this recorder's run identity.
    /// Native adapters may publish it only after applying the corresponding durable events.
    pub fn public_sequence(&self) -> Result<u64> {
        self.events
            .state
            .lock()
            .map(|state| state.sequence)
            .map_err(|_| anyhow!("application public sequence is unavailable"))
    }

    /// Rejoins the durable sequence without emitting a new start or acknowledging delivery.
    pub fn resume(session: &Session, run_id: &str) -> Result<Self> {
        let store = JsonlSessionStore::open_existing(
            session
                .store_path()
                .context("application recorder requires a durable session")?,
        )?;
        let records = store.read_event_records_writer()?;
        let mut events = ApplicationRunEventSequence::with_outbox_records(
            session.session_scope_id().to_owned(),
            run_id.to_owned(),
            store,
            &records,
        )?;
        let task_events = task_projector_from_records(&records)?;
        events.delivery_deferred = true;
        // No adapter callback or delivery receipt intervenes in deferred initialization.
        events.mark_live_delivery_prepared()?;
        Ok(Self {
            events,
            task_events,
            lifecycle: session.conversation_run_lifecycle_recorder()?,
            publication_uncertain: false,
        })
    }

    /// Records a foreground owner before it can expose provider or approval events.
    pub fn start(session: &Session, run_id: &str, prompt: &str) -> Result<Self> {
        let recorder = Self::resume(session, run_id)?;
        recorder
            .lifecycle
            .append_started(&ConversationRunStartedEntryV1::new(
                run_id,
                current_unix_time_ms(),
            )?)?;
        recorder.events.emit(
            &mut DeferredApplicationDelivery,
            PublicRunEventKind::RunStarted {
                prompt: safe_persistence_text(prompt),
            },
        )?;
        Ok(recorder)
    }

    fn with_bridge<T>(
        &mut self,
        operation: impl FnOnce(
            &mut PublicApplicationEventBridge<'_, DeferredApplicationDelivery>,
        ) -> Result<T>,
    ) -> Result<T> {
        let mut sink = DeferredApplicationDelivery;
        let mut bridge = PublicApplicationEventBridge {
            events: self.events.clone(),
            task_events: std::mem::take(&mut self.task_events),
            handler: &mut sink,
        };
        let result = operation(&mut bridge);
        self.task_events = bridge.task_events;
        if result
            .as_ref()
            .is_err_and(is_application_public_outbox_append_error)
        {
            self.publication_uncertain = true;
        }
        result
    }

    fn finish(
        &self,
        status: ApplicationRunTerminalStatus,
        final_message_id: Option<String>,
        summary: Option<&str>,
        event: PublicRunEventKind,
    ) -> Result<()> {
        if self.publication_uncertain {
            bail!(
                "application publication is uncertain; durable recovery must decide the terminal"
            );
        }
        if self
            .events
            .state
            .lock()
            .map_err(|_| anyhow!("application event state is unavailable"))?
            .terminal
        {
            return Ok(());
        }
        emit_application_conversation_terminal(
            &self.lifecycle,
            &self.events,
            &mut DeferredApplicationDelivery,
            &self.events.run_id,
            status,
            final_message_id,
            summary,
            &sigil_kernel::SecretRedactor::empty(),
            event,
        )
    }

    /// Commits the terminal corresponding to a completed kernel foreground run. Task handoffs
    /// retain their owner; the attached orchestration host must finalize their later result.
    pub fn finish_output(&self, output: &AgentRunOutput) -> Result<()> {
        if !matches!(
            output.disposition,
            AgentRunDisposition::FinalAnswer
                | AgentRunDisposition::AwaitingUserInput(_)
                | AgentRunDisposition::Interrupted
                | AgentRunDisposition::Blocked
        ) {
            return Ok(());
        }
        let (status, event) = application_terminal_projection(output);
        let final_message_id = (status == ApplicationRunTerminalStatus::Succeeded)
            .then(|| output.result.final_message_id.clone())
            .flatten();
        self.finish(status, final_message_id, None, event)
    }

    /// Finalizes a foreground handoff only after its Task owner produced a durable status.
    /// Completed answers are resolved and hash-checked through the same application projection
    /// used by the other runtime surfaces.
    pub fn finish_task(
        &self,
        session: &Session,
        task_id: &TaskId,
        status: TaskRunStatus,
    ) -> Result<()> {
        if status != TaskRunStatus::Paused {
            let (terminal, answer, event) =
                task_control::application_task_continuation_terminal(session, task_id, status)?;
            let summary = match &event {
                PublicRunEventKind::RunFailed { error } => Some(error.clone()),
                PublicRunEventKind::RunBlocked { reason }
                | PublicRunEventKind::RunPaused { reason }
                | PublicRunEventKind::RunInterrupted { reason } => Some(reason.clone()),
                _ => None,
            };
            return self.finish(
                terminal,
                answer.map(|answer| answer.message_id),
                summary.as_deref(),
                event,
            );
        }
        let output = application_task_terminal_output(
            session,
            task_id,
            status,
            AgentRunOutput {
                disposition: AgentRunDisposition::FinalAnswer,
                result: AgentRunResult {
                    final_text: String::new(),
                    tool_calls: 0,
                    final_message_id: None,
                },
                outcome: AgentRunOutcome::default(),
            },
        )?;
        self.finish_output(&output)
    }

    /// Closes a host-owned Plan wait or blocker after its coordinator returns that outcome.
    pub fn finish_blocked(&self, paused: bool, reason: &str) -> Result<()> {
        let reason = safe_persistence_text(reason);
        let (status, event) = if paused {
            (
                ApplicationRunTerminalStatus::Paused,
                PublicRunEventKind::RunPaused {
                    reason: reason.clone(),
                },
            )
        } else {
            (
                ApplicationRunTerminalStatus::Blocked,
                PublicRunEventKind::RunBlocked {
                    reason: reason.clone(),
                },
            )
        };
        self.finish(status, None, Some(&reason), event)
    }

    /// Publishes the current durable state of one input request owned by this root. An input
    /// decision can arrive after its foreground Awaiting terminal, so this narrow lifecycle
    /// update may advance the old stream without reopening or refinalizing that run.
    pub fn record_user_input_state(
        &self,
        session: &Session,
        identity: &sigil_kernel::UserInputIdentityV1,
    ) -> Result<()> {
        if session.session_scope_id() != self.events.session_id
            || session.store_path() != Some(self.events.outbox_store.path())
            || identity.session_scope_id.as_str() != self.events.session_id
            || identity.root_logical_run_id.as_str() != self.events.run_id
        {
            bail!("user input state does not belong to this durable public root");
        }
        let projection = session.user_input_projection()?;
        let request = projection
            .request(identity)
            .map(sigil_kernel::UserInputRequestStateV1::public_view)
            .context("durable user input state is missing")?;
        self.events.emit_nonterminal(
            &mut DeferredApplicationDelivery,
            user_input::application_user_input_changed_event(request),
            true,
        )
    }

    /// Records a provider/runtime error only when it is not an uncertain durable publication.
    pub fn finish_error(&self, error: &anyhow::Error) -> Result<()> {
        if self.publication_uncertain || is_application_public_outbox_append_error(error) {
            return Ok(());
        }
        let summary = safe_persistence_text(&format!("{error:#}"));
        let (status, event) =
            match error.downcast_ref::<sigil_kernel::ProviderTurnRecoveryTerminalError>() {
                Some(recovery) => application_provider_recovery_terminal(recovery, &summary),
                None => (
                    ApplicationRunTerminalStatus::Failed,
                    PublicRunEventKind::RunFailed {
                        error: summary.clone(),
                    },
                ),
            };
        self.finish(status, None, Some(&summary), event)
    }

    /// Called by the cancellation owner after its durable cleanup outcome is known.
    pub fn finish_cancelled(&self, interrupted: bool, reason: &str) -> Result<()> {
        let reason = safe_persistence_text(reason);
        let (status, event) = if interrupted {
            (
                ApplicationRunTerminalStatus::Interrupted,
                PublicRunEventKind::RunInterrupted {
                    reason: reason.clone(),
                },
            )
        } else {
            (
                ApplicationRunTerminalStatus::Cancelled,
                PublicRunEventKind::RunCancelled,
            )
        };
        self.finish(status, None, Some(&reason), event)
    }
}

impl EventHandler for ApplicationRunEventRecorder {
    fn begin_live_attempt(&mut self, physical_attempt_id: &str) -> Result<()> {
        self.events.live_preview.begin_attempt(physical_attempt_id)
    }

    fn handle(&mut self, event: RunEvent) -> Result<()> {
        self.with_bridge(|bridge| bridge.handle(event))
    }

    fn commit_controls(
        &mut self,
        session: &mut Session,
        controls: Vec<ControlEntry>,
    ) -> Result<Vec<sigil_kernel::StoredEvent>> {
        self.with_bridge(|bridge| bridge.commit_controls(session, controls))
    }

    fn commit_session_publications(
        &mut self,
        session: &mut Session,
        entries: Vec<SessionLogEntry>,
        publications: Vec<SessionPublicEventProjectionV1>,
    ) -> Result<Vec<sigil_kernel::StoredEvent>> {
        self.with_bridge(|bridge| {
            bridge.commit_session_publications(session, entries, publications)
        })
    }

    fn prepare_provider_output_publication(
        &mut self,
        session: &Session,
        control: &ControlEntry,
    ) -> Result<Option<ProviderOutputPublicationIntentV1>> {
        self.with_bridge(|bridge| bridge.prepare_provider_output_publication(session, control))
    }

    fn complete_provider_output_publication(
        &mut self,
        intent: Option<ProviderOutputPublicationIntentV1>,
        committed: Vec<PublicEventOutboxEntryV1>,
        event: RunEvent,
    ) -> Result<()> {
        self.with_bridge(|bridge| {
            bridge.complete_provider_output_publication(intent, committed, event)
        })
    }
}
