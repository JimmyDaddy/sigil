use anyhow::{Context, Result, bail};

use super::*;
use crate::{EventId, SessionId};

/// Durable acknowledgement outcome for a provider-continuation invalidation append.
///
/// Only the three present variants prove the exact terminal is durable. All other variants keep
/// the candidate and payload pinned; they do not authorize payload lifecycle changes, deletion,
/// fallback, boundary changes, or provider I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderContinuationCandidateInvalidationPersistence {
    /// The single-writer append and strict receipt validation succeeded.
    Recorded { event_id: EventId },
    /// The same terminal was already durable before this call.
    AlreadyPresent { event_id: EventId },
    /// A failed acknowledgement was reconciled to the exact durable terminal.
    ExactPresentAfterAckFailure { event_id: EventId },
    /// The failed append is proved absent; C3C2 must not start cleanup from this result.
    ConfirmedAbsentAfterAckFailure,
    /// A conflicting terminal is durable and the candidate remains quarantined.
    ConflictAfterAckFailure { reason: String },
    /// The durable stream cannot be safely reread and the candidate remains quarantined.
    IndeterminateAfterAckFailure { reason: String },
}

/// Single-writer C3C1 coordinator for one source-valid candidate invalidation terminal.
#[derive(Debug, Clone)]
pub struct ProviderContinuationCandidateInvalidationCoordinator {
    store: JsonlSessionStore,
}

impl ProviderContinuationCandidateInvalidationCoordinator {
    /// Creates a coordinator using the session's shared linear writer.
    #[must_use]
    pub fn new(store: JsonlSessionStore) -> Self {
        Self { store }
    }

    /// Appends one invalidation terminal or reconciles an ambiguous acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns an error when candidate/observation/plan/source-terminal provenance cannot be
    /// proven, a conflicting terminal is already known before append, or the prospective V2
    /// stream fails validation. No outcome of this method removes payload bytes.
    pub fn append_or_reconcile(
        &self,
        entry: ProviderContinuationCandidateInvalidatedEntry,
    ) -> Result<ProviderContinuationCandidateInvalidationPersistence> {
        let records = self.store.read_event_records_writer()?;
        let context = match prepare_invalidation_append(&records, &entry)? {
            InvalidationAppendPreparation::AlreadyPresent(existing) => {
                return Ok(
                    ProviderContinuationCandidateInvalidationPersistence::AlreadyPresent {
                        event_id: existing.event_id,
                    },
                );
            }
            InvalidationAppendPreparation::Ready(context) => context,
        };

        let event_id = provider_continuation_candidate_invalidated_event_id(&entry.candidate_id);
        let payload = serde_json::to_value(&entry)
            .context("failed to encode provider continuation invalidation")?;
        let record = DurableAuditRecord::new(
            DurableEventType::ProviderContinuationCandidateInvalidated,
            payload,
            entry.candidate_id.clone(),
            Some(context.correlation_id.clone()),
        )?
        .with_event_id(event_id.clone())?
        .with_causation_id(context.causation_id.clone())?;
        let reconciliation = record.reconciliation_expectation(context.session_id.clone())?;
        let expected = DurableAppendRecordExpectation::new(
            DurableEventType::ProviderContinuationCandidateInvalidated,
            entry.candidate_id.clone(),
            Some(context.correlation_id.clone()),
        )?
        .with_event_id(event_id.clone())?
        .with_causation_id(context.causation_id.clone())?;
        let receipt_expectation = DurableAppendExpectation::new(
            context.session_id.clone(),
            entry.candidate_id.clone(),
            vec![expected],
        )?;
        let batch = DurableAuditBatch::new(entry.candidate_id.clone(), vec![record])?;
        let expected_context = context.clone();
        let guard_entry = entry.clone();

        match self.store.append_audit_batch_if(batch, move |current| {
            match prepare_invalidation_append(current, &guard_entry)? {
                InvalidationAppendPreparation::AlreadyPresent(existing) => {
                    if existing.entry == guard_entry {
                        Ok(false)
                    } else {
                        bail!("provider continuation invalidation changed before durable append")
                    }
                }
                InvalidationAppendPreparation::Ready(current_context) => {
                    if current_context != expected_context {
                        bail!("provider continuation invalidation append frontier drifted")
                    }
                    validate_prospective_invalidation(current, &guard_entry, &current_context)?;
                    Ok(true)
                }
            }
        }) {
            Ok(Some(receipt)) => match DurableAuditWriter::validate_and_consume(
                &self.store,
                receipt,
                receipt_expectation,
            ) {
                Ok(_) => {
                    let state = self.exact_invalidation_state(&entry)?;
                    Ok(
                        ProviderContinuationCandidateInvalidationPersistence::Recorded {
                            event_id: state.event_id,
                        },
                    )
                }
                Err(_) => self.reconcile_acknowledgement(&entry, &reconciliation),
            },
            Ok(None) => {
                let state = self.exact_invalidation_state(&entry)?;
                Ok(
                    ProviderContinuationCandidateInvalidationPersistence::AlreadyPresent {
                        event_id: state.event_id,
                    },
                )
            }
            Err(_) => self.reconcile_acknowledgement(&entry, &reconciliation),
        }
    }

    fn reconcile_acknowledgement(
        &self,
        entry: &ProviderContinuationCandidateInvalidatedEntry,
        reconciliation: &DurableEventReconciliationExpectation,
    ) -> Result<ProviderContinuationCandidateInvalidationPersistence> {
        match self.store.reconcile_durable_event(reconciliation) {
            DurableEventReconciliation::ExactPresent(event) => {
                let state = self.exact_invalidation_state(entry)?;
                if state.event_id != event.event_id {
                    bail!("provider continuation invalidation reconciliation event id drifted")
                }
                Ok(
                    ProviderContinuationCandidateInvalidationPersistence::ExactPresentAfterAckFailure {
                        event_id: state.event_id,
                    },
                )
            }
            DurableEventReconciliation::ConfirmedAbsent => Ok(
                ProviderContinuationCandidateInvalidationPersistence::ConfirmedAbsentAfterAckFailure,
            ),
            DurableEventReconciliation::Conflict { reason } => Ok(
                ProviderContinuationCandidateInvalidationPersistence::ConflictAfterAckFailure {
                    reason,
                },
            ),
            DurableEventReconciliation::Indeterminate { reason } => Ok(
                ProviderContinuationCandidateInvalidationPersistence::IndeterminateAfterAckFailure {
                    reason,
                },
            ),
        }
    }

    fn exact_invalidation_state(
        &self,
        entry: &ProviderContinuationCandidateInvalidatedEntry,
    ) -> Result<ProviderContinuationCandidateInvalidationState> {
        let records = self.store.read_event_records_writer()?;
        let projection = ProviderContinuationProjection::from_records(&records)?;
        let state = projection
            .candidate_invalidation(&entry.candidate_id)
            .context("provider continuation invalidation is missing after acknowledgement")?;
        if state.entry != *entry {
            bail!("provider continuation invalidation conflicts with the durable record")
        }
        Ok(state.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InvalidationAppendPreparation {
    AlreadyPresent(Box<ProviderContinuationCandidateInvalidationState>),
    Ready(InvalidationAppendContext),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InvalidationAppendContext {
    session_id: SessionId,
    correlation_id: EventId,
    causation_id: EventId,
}

fn prepare_invalidation_append(
    records: &[SessionStreamRecord],
    entry: &ProviderContinuationCandidateInvalidatedEntry,
) -> Result<InvalidationAppendPreparation> {
    entry.validate_shape()?;
    let continuations = ProviderContinuationProjection::from_records(records)?;
    if let Some(existing) = continuations.candidate_invalidation(&entry.candidate_id) {
        if existing.entry == *entry {
            return Ok(InvalidationAppendPreparation::AlreadyPresent(Box::new(
                existing.clone(),
            )));
        }
        bail!("provider continuation candidate already has a conflicting invalidation")
    }
    let candidate = continuations
        .candidate(&entry.candidate_id)
        .with_context(|| {
            format!(
                "provider continuation invalidation references unknown candidate {}",
                entry.candidate_id
            )
        })?;
    let observation = continuations
        .observation(&entry.observation_id)
        .context("provider continuation invalidation references unknown observation")?;
    if entry.source_event_id != candidate.event_id || candidate.session_id != observation.session_id
    {
        bail!("provider continuation invalidation candidate source drifted")
    }
    let causation_id = match &entry.basis {
        ProviderContinuationCandidateInvalidationBasis::SourceOnly => candidate.event_id.clone(),
        ProviderContinuationCandidateInvalidationBasis::ResolutionPlan {
            resolution_plan_id,
        } => continuations
            .resolution_plan(resolution_plan_id)
            .with_context(|| {
                format!(
                    "provider continuation invalidation references unknown resolution plan {resolution_plan_id}"
                )
            })?
            .event_id
            .clone(),
    };
    let stream_session = stream_session_id(records)
        .context("provider continuation invalidation cannot append to an empty session")?;
    if stream_session != candidate.session_id {
        bail!("provider continuation invalidation does not belong to this session stream")
    }
    let context = InvalidationAppendContext {
        session_id: candidate.session_id.clone(),
        correlation_id: observation.correlation_id.clone(),
        causation_id,
    };
    validate_prospective_invalidation(records, entry, &context)?;
    Ok(InvalidationAppendPreparation::Ready(context))
}

fn validate_prospective_invalidation(
    records: &[SessionStreamRecord],
    entry: &ProviderContinuationCandidateInvalidatedEntry,
    context: &InvalidationAppendContext,
) -> Result<()> {
    let event_type = DurableEventType::ProviderContinuationCandidateInvalidated;
    let mut event = StoredEvent::new(
        event_type,
        event_type
            .expected_event_class()
            .context("provider continuation invalidation event has no expected class")?,
        provider_continuation_candidate_invalidated_event_id(&entry.candidate_id),
        context.session_id.clone(),
        next_stream_sequence(records),
        serde_json::to_value(entry)
            .context("failed to encode prospective provider continuation invalidation")?,
    )?;
    event.correlation_id = Some(context.correlation_id.clone());
    event.causation_id = Some(context.causation_id.clone());
    event.record_checksum = event.compute_record_checksum()?;
    let mut prospective = records.to_vec();
    prospective.push(SessionStreamRecord::Stored(event));
    ProviderContinuationProjection::from_records(&prospective)?;
    Ok(())
}
