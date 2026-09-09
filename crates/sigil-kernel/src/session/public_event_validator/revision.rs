use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::*;
use crate::{PlanDecision, PlanReviewAttemptEntry, PlanReviewAttemptStatus, PlanReviewProjection};

#[derive(Debug, Default)]
pub(super) struct RevisionValidationState {
    reviews: PlanReviewProjection,
    review_degraded: bool,
    started: BTreeSet<crate::PlanReviewAttemptId>,
    finalized: BTreeSet<String>,
    plans: BTreeMap<crate::PlanId, String>,
    decisions: BTreeMap<crate::PlanId, (PlanDecision, String)>,
    history: VecDeque<ArtifactControl>,
}

#[derive(Debug)]
enum ArtifactControl {
    Draft(Box<PlanDraftCreatedEntry>),
    Decision(PlanDecisionRecordedEntry),
    Resolution(crate::PlanReviewResolutionRecordedV1),
    Other,
}

#[derive(Debug)]
pub(super) enum RevisionPublicMaterial {
    Waiting(Box<PlanReviewAttemptEntry>),
    Terminal {
        attempt: Box<PlanReviewAttemptEntry>,
        draft: Option<Box<PlanDraftCreatedEntry>>,
        decision: PlanDecisionRecordedEntry,
    },
}

impl RevisionPublicMaterial {
    pub(super) fn validate(&self, event: &crate::PublicRunEvent) -> Result<()> {
        match self {
            Self::Waiting(attempt) => {
                super::super::plan_review_waiting::validate_waiting_material(attempt, event)
            }
            Self::Terminal {
                attempt,
                draft,
                decision,
            } => super::super::plan_review_terminal::validate_terminal_material(
                attempt,
                draft.as_deref(),
                decision,
                event,
            ),
        }
    }
}

impl RevisionValidationState {
    pub(super) fn apply_entry(
        &mut self,
        entry: &SessionLogEntry,
    ) -> Result<Option<RevisionPublicMaterial>> {
        let SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt)) = entry else {
            return Ok(None);
        };
        let material = if super::super::plan_review_terminal::is_revision_terminal(attempt) {
            if !self
                .finalized
                .insert(super::super::plan_review_terminal::plan_review_revision_run_id(attempt))
            {
                bail!("revision has multiple terminal outcomes");
            }
            let success = attempt.status == PlanReviewAttemptStatus::DraftReady;
            let has_resolution = matches!(
                self.history.iter().rev().nth(1),
                Some(ArtifactControl::Resolution(_))
            );
            self.flush_history_except(if success {
                if has_resolution { 3 } else { 2 }
            } else {
                1
            });
            let Some(ArtifactControl::Decision(decision)) = self.history.back() else {
                bail!("revision terminal lost its adjacent base decision");
            };
            if decision.decided_at_ms != attempt.recorded_at_ms {
                bail!("revision bundle has inconsistent original commit timestamps");
            }
            let draft = if success {
                let Some(ArtifactControl::Draft(draft)) = self.history.front() else {
                    bail!("revision success lost its adjacent draft");
                };
                if let Some(ArtifactControl::Resolution(resolution)) = self.history.get(1) {
                    super::super::plan_review_terminal::validate_terminal_resolution(
                        attempt, draft, resolution,
                    )?;
                }
                Some(draft.clone())
            } else {
                None
            };
            self.validate_predecessor(attempt)?;
            Some(RevisionPublicMaterial::Terminal {
                attempt: Box::new(attempt.clone()),
                draft,
                decision: decision.clone(),
            })
        } else if super::super::plan_review_waiting::is_revision_waiting(attempt) {
            self.flush_history_except(0);
            self.validate_predecessor(attempt)?;
            Some(RevisionPublicMaterial::Waiting(Box::new(attempt.clone())))
        } else {
            None
        };
        if attempt.status == PlanReviewAttemptStatus::Started {
            self.started.insert(attempt.attempt_id.clone());
        }
        if self
            .reviews
            .apply_public_validation_metadata(attempt)
            .is_err()
        {
            self.review_degraded = true;
        }
        Ok(material)
    }

    fn validate_predecessor(&self, attempt: &PlanReviewAttemptEntry) -> Result<()> {
        if self.review_degraded || self.reviews.has_conflicts() {
            bail!("revision predecessor has conflicting lifecycle facts");
        }
        if !self.started.contains(&attempt.attempt_id) {
            bail!("revision terminal requires a matching actual Started attempt");
        }
        let compact = crate::conversation_route::public_validation_attempt_metadata(attempt)?;
        let mut expected = self
            .reviews
            .latest_attempt(&attempt.plan_review_id)
            .context("revision has no current attempt")?
            .clone();
        expected.status = compact.status;
        expected.terminal_reason = compact.terminal_reason;
        expected.pending_user_input = compact.pending_user_input.clone();
        expected.recorded_at_ms = compact.recorded_at_ms;
        if expected != compact {
            bail!("revision terminal changes its immutable attempt binding");
        }
        self.reviews.validate_append(&compact)?;
        let base = attempt
            .base_plan_id
            .as_ref()
            .context("revision has no base plan")?;
        let hash = attempt
            .base_plan_hash
            .as_ref()
            .context("revision has no base hash")?;
        if self.plans.get(base) != Some(hash)
            || self.decisions.get(base) != Some(&(PlanDecision::RevisionRequested, hash.clone()))
            || self.plans.contains_key(&attempt.plan_id)
        {
            bail!("revision terminal requires exact pending base and no split candidate draft");
        }
        Ok(())
    }

    /// Only three adjacent bundle controls are delayed; drafts keep identity/lineage, no content.
    pub(super) fn advance_history(&mut self, record: &SessionStreamRecord) -> Result<()> {
        let update = match record.session_log_entry()? {
            Some(SessionLogEntry::Control(ControlEntry::PlanDraftCreated(mut draft))) => {
                draft.summary.clear();
                draft.inline_text = None;
                draft.steps.clear();
                draft.intent_proposal = None;
                draft.target_paths.clear();
                draft.suggested_checks.clear();
                draft.risk = None;
                draft.notes.clear();
                ArtifactControl::Draft(Box::new(draft))
            }
            Some(SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(mut decision))) => {
                decision.reason = None;
                ArtifactControl::Decision(decision)
            }
            Some(SessionLogEntry::Control(ControlEntry::PlanReviewResolutionRecordedV1(
                resolution,
            ))) => ArtifactControl::Resolution(resolution),
            _ => ArtifactControl::Other,
        };
        self.history.push_back(update);
        self.flush_history_except(3);
        Ok(())
    }

    fn flush_history_except(&mut self, keep: usize) {
        while self.history.len() > keep {
            match self
                .history
                .pop_front()
                .expect("history has a pending item")
            {
                ArtifactControl::Draft(draft) => {
                    self.plans.insert(draft.plan_id, draft.plan_hash);
                }
                ArtifactControl::Decision(decision) => {
                    self.decisions
                        .insert(decision.plan_id, (decision.decision, decision.plan_hash));
                }
                ArtifactControl::Other | ArtifactControl::Resolution(_) => {}
            }
        }
    }
}
