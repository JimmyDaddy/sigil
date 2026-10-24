use super::*;

/// Only scalar metadata is retained in the incremental index. The bounded public view is
/// hydrated from its exact committed outbox source at the page's fixed cut.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ProviderRecoveryProjection {
    run_id: Option<String>,
    run_sequence: u64,
    source_sequence: Option<u64>,
    settled: bool,
}

impl ProviderRecoveryProjection {
    pub(super) fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        if let Some(lifecycle) = conversation_run_lifecycle_record_from_stream(record)? {
            match lifecycle {
                ConversationRunLifecycleRecordV1::ConversationRunStartedV1(started) => {
                    *self = Self {
                        run_id: Some(started.run_id().to_owned()),
                        ..Self::default()
                    };
                }
                ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(finalized)
                    if self.run_id.as_deref() == Some(finalized.run_id())
                        && matches!(
                            finalized.status(),
                            ConversationRunTerminalStatusV1::Succeeded
                                | ConversationRunTerminalStatusV1::Cancelled
                                | ConversationRunTerminalStatusV1::AwaitingUserInput
                        ) =>
                {
                    self.source_sequence = None;
                    self.settled = true;
                }
                _ => {}
            }
        }
        if record.stored_event().event_kind()
            == Some(DurableEventType::ProviderTurnRecoveryExhausted)
        {
            let exhausted: sigil_kernel::ProviderTurnRecoveryExhaustedEntry =
                serde_json::from_value(record.stored_event().payload.clone())?;
            if self.run_id.as_deref() == Some(exhausted.logical_run_id.as_str())
                && exhausted.terminal_disposition
                    == sigil_kernel::ProviderTurnRecoveryTerminalDispositionV1::Cancelled
            {
                self.source_sequence = None;
                self.settled = true;
            }
        }
        if record.stored_event().event_type != DurableEventType::PublicEventOutbox.as_str() {
            return Ok(());
        }
        let outbox: sigil_kernel::PublicEventOutboxEntryV1 =
            serde_json::from_value(record.stored_event().payload.clone())?;
        if self.run_id.as_deref() != Some(outbox.run_id.as_str()) {
            return Ok(());
        }
        self.run_sequence = self.run_sequence.max(outbox.sequence);
        match outbox.event.event {
            PublicRunEventKind::ProviderTurnRecoveryChanged { recovery } if !self.settled => {
                self.source_sequence = actionable(&recovery).then_some(record.stream_sequence());
            }
            PublicRunEventKind::RunCancelled | PublicRunEventKind::RunFinished { .. } => {
                self.source_sequence = None;
                self.settled = true;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn source_sequence(&self) -> Option<u64> {
        self.source_sequence
    }

    pub(super) fn hydrate<'a>(
        &self,
        source: impl FnOnce(u64) -> Result<&'a SessionStreamRecord>,
    ) -> Result<Option<ConversationProviderRecoveryV1>> {
        let Some(run_id) = &self.run_id else {
            return Ok(None);
        };
        let recovery = self
            .source_sequence
            .map(|sequence| {
                let record = source(sequence)?;
                let outbox: sigil_kernel::PublicEventOutboxEntryV1 =
                    serde_json::from_value(record.stored_event().payload.clone())?;
                let PublicRunEventKind::ProviderTurnRecoveryChanged { recovery } =
                    outbox.event.event
                else {
                    bail!("conversation recovery source changed type");
                };
                if record.stream_sequence() != sequence
                    || outbox.run_id != *run_id
                    || outbox.sequence > self.run_sequence
                    || !actionable(&recovery)
                {
                    bail!("conversation recovery source changed identity");
                }
                Ok::<_, anyhow::Error>(recovery)
            })
            .transpose()?;
        Ok(Some(ConversationProviderRecoveryV1 {
            run_id: run_id.clone(),
            run_sequence: self.run_sequence,
            recovery,
        }))
    }
}

fn actionable(recovery: &sigil_kernel::PublicProviderTurnRecoveryViewV1) -> bool {
    recovery.user_attention_required
        && matches!(
            recovery.phase,
            sigil_kernel::PublicProviderTurnRecoveryPhaseV1::Blocked
                | sigil_kernel::PublicProviderTurnRecoveryPhaseV1::Paused
        )
}
