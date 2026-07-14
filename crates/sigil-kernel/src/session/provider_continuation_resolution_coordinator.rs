use anyhow::{Context, Result, bail};

use super::*;
use crate::{EventId, SessionId};

/// Durable acknowledgement outcome for a provider-observed resolution-plan append.
///
/// Only [`Recorded`](Self::Recorded), [`AlreadyPresent`](Self::AlreadyPresent), and
/// [`ExactPresentAfterAckFailure`](Self::ExactPresentAfterAckFailure) prove that the exact frozen
/// plan is durable. All other outcomes leave the candidate pinned and inactive; they do not
/// authorize cleanup, an invalidation terminal, provider I/O, or a compaction boundary change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderObservedResolutionPlanPersistence {
    /// The strict single-writer append and receipt validation succeeded.
    Recorded { event_id: EventId },
    /// The exact plan was already durable before this call acquired the write boundary.
    AlreadyPresent { event_id: EventId },
    /// The writer acknowledgement failed, but a locked reread found the exact durable plan.
    ExactPresentAfterAckFailure { event_id: EventId },
    /// The failed append was proved absent. The candidate remains pinned pending C3B handling.
    ConfirmedAbsentAfterAckFailure,
    /// The failed append conflicts with a different durable record and remains quarantined.
    ConflictAfterAckFailure { reason: String },
    /// The durable stream could not be safely reread, so the candidate remains quarantined.
    IndeterminateAfterAckFailure { reason: String },
}

/// Single-writer C3A coordinator for a frozen provider-observed resolution plan.
///
/// The coordinator has one deliberately narrow responsibility: append the one preallocated plan
/// event after its source attempt completed and its durable activation gate closed, then reconcile
/// an ambiguous acknowledgement by exact event identity. It intentionally does not materialize a
/// payload, validate/apply a compaction boundary, write invalidation, delete storage, or call a
/// provider.
#[derive(Debug, Clone)]
pub struct ProviderObservedResolutionPlanCoordinator {
    store: JsonlSessionStore,
}

impl ProviderObservedResolutionPlanCoordinator {
    /// Creates a coordinator rooted at one session's shared linear writer.
    #[must_use]
    pub fn new(store: JsonlSessionStore) -> Self {
        Self { store }
    }

    /// Appends one exact provider-observed plan or reconciles its acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns an error when the plan is malformed, its source terminal/gate is not proven, the
    /// plan conflicts with the durable frontier, or the prospective V2 stream would fail closed.
    /// Ambiguous writer acknowledgements are returned as
    /// [`ProviderObservedResolutionPlanPersistence`] rather than guessed as success or absence.
    pub fn append_or_reconcile(
        &self,
        plan: ProviderObservedResolutionPlanRecordedEntry,
    ) -> Result<ProviderObservedResolutionPlanPersistence> {
        let records = self.store.read_event_records_writer()?;
        let context = match prepare_plan_append(&records, &plan)? {
            PlanAppendPreparation::AlreadyPresent(existing) => {
                return Ok(ProviderObservedResolutionPlanPersistence::AlreadyPresent {
                    event_id: existing.event_id,
                });
            }
            PlanAppendPreparation::Ready(context) => context,
        };

        let event_id =
            provider_observed_resolution_plan_recorded_event_id(&plan.resolution_plan_id);
        let payload = serde_json::to_value(&plan)
            .context("failed to encode provider-observed resolution plan")?;
        let record = DurableAuditRecord::new(
            DurableEventType::ProviderObservedResolutionPlanRecorded,
            payload.clone(),
            plan.resolution_plan_id.clone(),
            Some(context.correlation_id.clone()),
        )?
        .with_event_id(event_id.clone())?
        .with_causation_id(context.causation_id.clone())?;
        let reconciliation = record.reconciliation_expectation(context.session_id.clone())?;
        let expected = DurableAppendRecordExpectation::new(
            DurableEventType::ProviderObservedResolutionPlanRecorded,
            plan.resolution_plan_id.clone(),
            Some(context.correlation_id.clone()),
        )?
        .with_event_id(event_id.clone())?
        .with_causation_id(context.causation_id.clone())?;
        let receipt_expectation = DurableAppendExpectation::new(
            context.session_id.clone(),
            plan.resolution_plan_id.clone(),
            vec![expected],
        )?;
        let batch = DurableAuditBatch::new(plan.resolution_plan_id.clone(), vec![record])?;
        let expected_context = context.clone();
        let guard_plan = plan.clone();

        match self.store.append_audit_batch_if(batch, move |current| {
            match prepare_plan_append(current, &guard_plan)? {
                PlanAppendPreparation::AlreadyPresent(existing) => {
                    if existing.entry == guard_plan {
                        Ok(false)
                    } else {
                        bail!("provider-observed resolution plan changed before durable append")
                    }
                }
                PlanAppendPreparation::Ready(current_context) => {
                    if current_context != expected_context {
                        bail!("provider-observed resolution plan append frontier drifted")
                    }
                    validate_prospective_plan(current, &guard_plan, &current_context)?;
                    Ok(true)
                }
            }
        }) {
            Ok(Some(receipt)) => {
                match DurableAuditWriter::validate_and_consume(
                    &self.store,
                    receipt,
                    receipt_expectation,
                ) {
                    Ok(_) => {
                        let state = self.exact_plan_state(&plan)?;
                        Ok(ProviderObservedResolutionPlanPersistence::Recorded {
                            event_id: state.event_id,
                        })
                    }
                    Err(_) => self.reconcile_acknowledgement(&plan, &reconciliation),
                }
            }
            Ok(None) => {
                let state = self.exact_plan_state(&plan)?;
                Ok(ProviderObservedResolutionPlanPersistence::AlreadyPresent {
                    event_id: state.event_id,
                })
            }
            Err(_) => self.reconcile_acknowledgement(&plan, &reconciliation),
        }
    }

    fn reconcile_acknowledgement(
        &self,
        plan: &ProviderObservedResolutionPlanRecordedEntry,
        reconciliation: &DurableEventReconciliationExpectation,
    ) -> Result<ProviderObservedResolutionPlanPersistence> {
        match self.store.reconcile_durable_event(reconciliation) {
            DurableEventReconciliation::ExactPresent(event) => {
                let state = self.exact_plan_state(plan)?;
                if state.event_id != event.event_id {
                    bail!("provider-observed resolution plan reconciliation event id drifted")
                }
                Ok(
                    ProviderObservedResolutionPlanPersistence::ExactPresentAfterAckFailure {
                        event_id: state.event_id,
                    },
                )
            }
            DurableEventReconciliation::ConfirmedAbsent => {
                Ok(ProviderObservedResolutionPlanPersistence::ConfirmedAbsentAfterAckFailure)
            }
            DurableEventReconciliation::Conflict { reason } => {
                Ok(ProviderObservedResolutionPlanPersistence::ConflictAfterAckFailure { reason })
            }
            DurableEventReconciliation::Indeterminate { reason } => Ok(
                ProviderObservedResolutionPlanPersistence::IndeterminateAfterAckFailure { reason },
            ),
        }
    }

    fn exact_plan_state(
        &self,
        plan: &ProviderObservedResolutionPlanRecordedEntry,
    ) -> Result<ProviderObservedResolutionPlanState> {
        let records = self.store.read_event_records_writer()?;
        let projection = ProviderContinuationProjection::from_records(&records)?;
        let state = projection
            .resolution_plan(&plan.resolution_plan_id)
            .context("provider-observed resolution plan is missing after acknowledgement")?;
        if state.entry != *plan {
            bail!("provider-observed resolution plan conflicts with the durable record")
        }
        Ok(state.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PlanAppendPreparation {
    AlreadyPresent(Box<ProviderObservedResolutionPlanState>),
    Ready(PlanAppendContext),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlanAppendContext {
    session_id: SessionId,
    correlation_id: EventId,
    causation_id: EventId,
}

fn prepare_plan_append(
    records: &[SessionStreamRecord],
    plan: &ProviderObservedResolutionPlanRecordedEntry,
) -> Result<PlanAppendPreparation> {
    plan.validate_shape()?;
    let continuations = ProviderContinuationProjection::from_records(records)?;
    if let Some(existing) = continuations.resolution_plan_for_candidate(&plan.candidate_id) {
        if existing.entry == *plan {
            return Ok(PlanAppendPreparation::AlreadyPresent(Box::new(
                existing.clone(),
            )));
        }
        bail!("provider continuation candidate already has a conflicting resolution plan")
    }

    let candidate = continuations
        .candidate(&plan.candidate_id)
        .with_context(|| {
            format!(
                "provider-observed resolution plan references unknown candidate {}",
                plan.candidate_id
            )
        })?;
    let observation = continuations
        .observation(&plan.observation_id)
        .context("provider-observed resolution plan references unknown observation")?;
    if candidate.session_id != observation.session_id {
        bail!("provider-observed resolution plan candidate session drifted")
    }
    let physical_attempts = ProviderPhysicalAttemptProjection::from_records(records)?;
    let source_attempt = physical_attempts
        .attempt(&observation.entry.physical_attempt_id)
        .context("provider-observed resolution plan source physical attempt is missing")?;
    let source_terminal = source_attempt
        .terminal
        .as_ref()
        .context("provider-observed resolution plan source terminal is not durable")?;
    if source_terminal.outcome != ProviderPhysicalAttemptOutcome::Completed {
        bail!("provider-observed resolution plan source terminal did not complete")
    }
    let terminal_sequence = source_attempt
        .terminal_stream_sequence
        .context("provider-observed resolution plan source terminal has no sequence")?;
    let next_sequence = next_stream_sequence(records);
    if terminal_sequence >= next_sequence {
        bail!("provider-observed resolution plan source terminal is not before the append")
    }
    let (session_id, correlation_id, causation_id) =
        continuations.resolution_plan_append_links(&plan.candidate_id)?;
    if session_id != candidate.session_id {
        bail!("provider-observed resolution plan append session drifted")
    }
    let stream_session = stream_session_id(records)
        .context("provider-observed resolution plan cannot append to an empty session")?;
    if stream_session != session_id {
        bail!("provider-observed resolution plan does not belong to this session stream")
    }
    let context = PlanAppendContext {
        session_id,
        correlation_id,
        causation_id,
    };
    validate_prospective_plan(records, plan, &context)?;
    Ok(PlanAppendPreparation::Ready(context))
}

fn validate_prospective_plan(
    records: &[SessionStreamRecord],
    plan: &ProviderObservedResolutionPlanRecordedEntry,
    context: &PlanAppendContext,
) -> Result<()> {
    let event_type = DurableEventType::ProviderObservedResolutionPlanRecorded;
    let mut event = StoredEvent::new(
        event_type,
        event_type
            .expected_event_class()
            .context("resolution plan event has no expected class")?,
        provider_observed_resolution_plan_recorded_event_id(&plan.resolution_plan_id),
        context.session_id.clone(),
        next_stream_sequence(records),
        serde_json::to_value(plan).context("failed to encode prospective resolution plan")?,
    )?;
    event.correlation_id = Some(context.correlation_id.clone());
    event.causation_id = Some(context.causation_id.clone());
    event.record_checksum = event.compute_record_checksum()?;
    let mut prospective = records.to_vec();
    prospective.push(SessionStreamRecord::Stored(event));
    ProviderContinuationProjection::from_records(&prospective)?;
    Ok(())
}
