//! Strict incremental counterpart of durable public-outbox replay validation.

use std::collections::BTreeSet;

use super::*;
use crate::{
    ConversationRunLifecycleRecordV1, PublicRunEventKind, PublicTaskEventProjector,
    projection_apply_decision,
};

mod revision;
use revision::RevisionValidationState;

/// Validates one forward durable prefix without retaining historical public payloads.
///
/// The caller feeds every record, including private/domain records, and calls `validate_cut`
/// only after reaching its fixed read cut. Adjacent bundles may span bounded read batches.
/// An error invalidates this validator; callers must discard it rather than continue applying.
#[derive(Debug, Default)]
pub struct PublicEventOutboxValidatorV1 {
    cursor: Option<ProjectionCursor>,
    outbox: PublicEventOutboxAdmissionIndexV1,
    started_runs: BTreeSet<String>,
    active_conversation: Option<String>,
    projector: PublicTaskEventProjector,
    projector_degraded: bool,
    has_linked_publication: bool,
    revision: RevisionValidationState,
    pending: Option<PendingSource>,
}

#[derive(Debug)]
struct SourceEnvelope {
    event_id: String,
    session_id: String,
    sequence: u64,
    correlation_id: Option<String>,
}

impl From<&StoredEvent> for SourceEnvelope {
    fn from(event: &StoredEvent) -> Self {
        Self {
            event_id: event.event_id.clone(),
            session_id: event.session_id.clone(),
            sequence: event.stream_sequence,
            correlation_id: event.correlation_id.clone(),
        }
    }
}

#[derive(Debug)]
struct PendingSource {
    envelope: SourceEnvelope,
    material: SourceMaterial,
}

#[derive(Debug)]
enum SourceMaterial {
    ConversationTerminal(crate::ConversationRunFinalizedEntryV1),
    Revision(revision::RevisionPublicMaterial),
    Optional {
        explicit: Option<super::control_publication::ExplicitSessionPublicationFingerprint>,
        expected: Option<Vec<String>>,
        source_kind_valid: bool,
        published: usize,
        run_id: Option<String>,
    },
}

impl PublicEventOutboxValidatorV1 {
    pub fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        let event = record.stored_event();
        if projection_apply_decision(self.cursor.as_ref(), event)?
            == ProjectionApplyDecision::IgnoreAlreadyApplied
        {
            return Ok(());
        }
        decode_stored_event(event.clone())?;
        self.outbox.apply_record(record)?;
        if event.event_kind() == Some(DurableEventType::PublicEventOutbox) {
            let entry: PublicEventOutboxEntryV1 = serde_json::from_value(event.payload.clone())?;
            self.apply_public(event, &entry)?;
        } else {
            self.finish_pending()?;
            self.pending = None;
            self.apply_domain(record)?;
        }
        self.revision.advance_history(record)?;
        self.cursor = Some(record.projection_cursor(PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION));
        Ok(())
    }

    /// Verifies complete bundles after the reader reaches its captured durable high watermark.
    pub fn validate_cut(&self) -> Result<()> {
        self.finish_pending()?;
        if self.has_linked_publication && self.projector_degraded {
            bail!("public control projection has a corrupt domain lifecycle");
        }
        Ok(())
    }

    #[must_use]
    pub fn durable_sequence(&self, run_id: &str) -> u64 {
        self.outbox.durable_sequence(run_id)
    }

    #[must_use]
    pub fn contains_event(&self, event_id: &str) -> bool {
        self.outbox.contains_event(event_id)
    }

    #[must_use]
    pub fn was_delivered(&self, event_id: &str, adapter: &str) -> bool {
        self.outbox.was_delivered(event_id, adapter)
    }

    fn apply_domain(&mut self, record: &SessionStreamRecord) -> Result<()> {
        let event = record.stored_event();
        if let Some(lifecycle) = crate::conversation_run_lifecycle_record_from_stream(record)? {
            match lifecycle {
                ConversationRunLifecycleRecordV1::ConversationRunStartedV1(started) => {
                    if !self.started_runs.insert(started.run_id().to_owned()) {
                        bail!("conversation run stream contains duplicate starts");
                    }
                    if self
                        .active_conversation
                        .replace(started.run_id().to_owned())
                        .is_some()
                    {
                        bail!("conversation run stream contains overlapping active runs");
                    }
                }
                ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(terminal) => {
                    if self.active_conversation.as_deref() != Some(terminal.run_id()) {
                        bail!("conversation run terminal has no matching active start");
                    }
                    self.active_conversation = None;
                    self.pending = Some(PendingSource {
                        envelope: event.into(),
                        material: SourceMaterial::ConversationTerminal(terminal),
                    });
                }
            }
            return Ok(());
        }
        let Some(entry) = record.session_log_entry()? else {
            return Ok(());
        };
        let revision = self.revision.apply_entry(&entry)?;
        let source_kind_valid =
            event.event_kind() == Some(super::store::session_entry_event_type(&entry));
        let projected = if let SessionLogEntry::Control(control) = &entry {
            match self.projector.project_control(control) {
                Ok(projected) => super::control_publication::finish_explicit_control_projection(
                    control, projected,
                )
                .ok(),
                Err(_) => {
                    self.projector_degraded = true;
                    None
                }
            }
        } else {
            None
        };
        if let Some(revision) = revision {
            if !source_kind_valid {
                bail!("public revision source uses an incorrect durable event type");
            }
            self.pending = Some(PendingSource {
                envelope: event.into(),
                material: SourceMaterial::Revision(revision),
            });
            return Ok(());
        }
        let explicit =
            super::control_publication::ExplicitSessionPublicationFingerprint::from_entry(&entry)?;
        let expected = projected
            .map(|events| events.iter().map(event_digest).collect::<Result<Vec<_>>>())
            .transpose()?;
        self.pending = Some(PendingSource {
            envelope: event.into(),
            material: SourceMaterial::Optional {
                explicit,
                expected,
                source_kind_valid,
                published: 0,
                run_id: None,
            },
        });
        Ok(())
    }

    fn apply_public(
        &mut self,
        envelope: &StoredEvent,
        outbox: &PublicEventOutboxEntryV1,
    ) -> Result<()> {
        let terminal = super::public_event_outbox::is_terminal_event(&outbox.event.event);
        if !terminal && outbox.domain_event_id == outbox.public_event_id {
            self.finish_pending()?;
            self.pending = None;
            return Ok(());
        }
        let source = self
            .pending
            .as_mut()
            .context("public event has no exact adjacent durable source")?;
        if source.envelope.event_id != outbox.domain_event_id
            || source.envelope.session_id != outbox.event.session_id
            || envelope.session_id != outbox.event.session_id
        {
            bail!("public event does not match its exact durable source identity");
        }
        match &mut source.material {
            SourceMaterial::ConversationTerminal(material) => {
                validate_adjacent_pair(&source.envelope, envelope)?;
                crate::conversation_run::validate_terminal_outbox(material, outbox)?;
                self.pending = None;
            }
            SourceMaterial::Revision(material) => {
                validate_adjacent_pair(&source.envelope, envelope)?;
                material.validate(&outbox.event)?;
                self.pending = None;
            }
            SourceMaterial::Optional {
                explicit,
                expected,
                source_kind_valid,
                published,
                run_id,
            } => {
                if terminal || !*source_kind_valid {
                    bail!("public session source has no active exact publication contract");
                }
                self.outbox.require_active_run(&outbox.run_id)?;
                if self.projector_degraded {
                    bail!("public control projection has a corrupt domain lifecycle");
                }
                self.has_linked_publication = true;
                if envelope.causation_id.as_deref() != Some(source.envelope.event_id.as_str())
                    || envelope.correlation_id != source.envelope.correlation_id
                    || run_id
                        .as_ref()
                        .is_some_and(|run_id| run_id != &outbox.run_id)
                {
                    bail!("public session publication does not match its durable envelope");
                }
                if let Some(explicit) = explicit {
                    if *published != 0 {
                        bail!("explicit public session projection must have exactly one DTO");
                    }
                    explicit.validate(&outbox.event.event)?;
                } else if expected.as_ref().and_then(|events| events.get(*published))
                    != Some(&event_digest(&outbox.event.event)?)
                {
                    bail!("public control publication does not match its exact source projection");
                }
                *published += 1;
                *run_id = Some(outbox.run_id.clone());
            }
        }
        Ok(())
    }

    fn finish_pending(&self) -> Result<()> {
        let Some(source) = &self.pending else {
            return Ok(());
        };
        match &source.material {
            SourceMaterial::Optional {
                explicit,
                expected,
                published,
                ..
            } => {
                if *published != 0
                    && *published
                        != if explicit.is_some() {
                            1
                        } else {
                            expected.as_ref().map_or(0, Vec::len)
                        }
                {
                    bail!("public control publication is missing part of its source projection");
                }
            }
            _ => bail!("durable terminal or suspension has no exact public outbox pair"),
        }
        Ok(())
    }
}

fn validate_adjacent_pair(source: &SourceEnvelope, public: &StoredEvent) -> Result<()> {
    if source.sequence.checked_add(1) != Some(public.stream_sequence) {
        bail!("public terminal and domain do not form one exact durable pair");
    }
    Ok(())
}

fn event_digest(event: &PublicRunEventKind) -> Result<String> {
    Ok(stable_event_hash(serde_json::to_vec(event)?))
}

#[cfg(test)]
#[path = "tests/public_event_validator_tests.rs"]
mod tests;
