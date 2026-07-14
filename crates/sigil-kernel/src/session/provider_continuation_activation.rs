use anyhow::Result;

use super::*;

/// Read-only activation state for one durable continuation candidate.
///
/// `Ready` means only that its durable tool-closure gate is satisfied. It never activates a
/// provider projection, sends a provider request, or changes a compaction boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderContinuationActivationState {
    Ready {
        candidate_id: ProviderContinuationCandidateId,
    },
    AwaitingToolClosure {
        candidate_id: ProviderContinuationCandidateId,
        pending_tool_calls: Vec<ProviderToolCallClosureRef>,
        lease_expires_at_unix_ms: u64,
    },
    LeaseExpired {
        candidate_id: ProviderContinuationCandidateId,
        pending_tool_calls: Vec<ProviderToolCallClosureRef>,
        lease_expires_at_unix_ms: u64,
    },
}

/// Pure evaluator for candidate activation gates at one explicit wall-clock instant.
pub struct ProviderContinuationActivationEvaluator;

impl ProviderContinuationActivationEvaluator {
    /// Evaluates all durable candidates at `now_unix_ms` without mutating the session.
    ///
    /// # Errors
    ///
    /// Returns an error when the V2 stream or the candidate/closure provenance is invalid.
    pub fn from_records_at(
        records: &[SessionStreamRecord],
        now_unix_ms: u64,
    ) -> Result<Vec<ProviderContinuationActivationState>> {
        let projection = ProviderContinuationProjection::from_records(records)?;
        Ok(projection
            .candidates()
            .map(|candidate| evaluate_candidate(&projection, candidate, now_unix_ms))
            .collect())
    }
}

impl JsonlSessionStore {
    /// Reads durable candidate activation states at an explicit time without writing lease expiry,
    /// cleanup, or resolution records.
    ///
    /// # Errors
    ///
    /// Returns an error when the current V2 stream cannot prove candidate/closure provenance.
    pub fn provider_continuation_activation_at(
        &self,
        now_unix_ms: u64,
    ) -> Result<Vec<ProviderContinuationActivationState>> {
        let records = Self::read_event_records(self.path())?;
        ProviderContinuationActivationEvaluator::from_records_at(&records, now_unix_ms)
    }
}

fn evaluate_candidate(
    projection: &ProviderContinuationProjection,
    candidate: &ProviderContinuationCandidateState,
    now_unix_ms: u64,
) -> ProviderContinuationActivationState {
    match &candidate.entry.activation_gate {
        ProviderContinuationActivationGate::Immediate => {
            ProviderContinuationActivationState::Ready {
                candidate_id: candidate.entry.candidate_id.clone(),
            }
        }
        ProviderContinuationActivationGate::AwaitingToolClosure {
            tool_calls,
            lease_expires_at_unix_ms,
        } => {
            let closures = projection.tool_closures_for_candidate(&candidate.entry.candidate_id);
            let pending_tool_calls = tool_calls
                .iter()
                .filter(|tool_call| {
                    !closures
                        .iter()
                        .any(|closure| &closure.entry.tool_call == *tool_call)
                })
                .cloned()
                .collect::<Vec<_>>();
            if pending_tool_calls.is_empty() {
                ProviderContinuationActivationState::Ready {
                    candidate_id: candidate.entry.candidate_id.clone(),
                }
            } else if now_unix_ms > *lease_expires_at_unix_ms {
                ProviderContinuationActivationState::LeaseExpired {
                    candidate_id: candidate.entry.candidate_id.clone(),
                    pending_tool_calls,
                    lease_expires_at_unix_ms: *lease_expires_at_unix_ms,
                }
            } else {
                ProviderContinuationActivationState::AwaitingToolClosure {
                    candidate_id: candidate.entry.candidate_id.clone(),
                    pending_tool_calls,
                    lease_expires_at_unix_ms: *lease_expires_at_unix_ms,
                }
            }
        }
    }
}
