//! Revision terminal transactions use the existing attempt as their sole domain record.

use super::*;
use crate::{
    PlanDecision, PlanDecisionActor, PlanReviewAttemptEntry, PlanReviewAttemptStatus,
    PlanReviewProjection, PublicRunEvent, PublicRunEventKind,
};

/// The execution identity already shared by revision callers and their child session.
#[must_use]
pub fn plan_review_revision_run_id(attempt: &PlanReviewAttemptEntry) -> String {
    format!(
        "plan-review-{}-{}",
        attempt.plan_review_id.as_str(),
        attempt.attempt_id.as_str()
    )
}

pub(super) fn is_revision_terminal(attempt: &PlanReviewAttemptEntry) -> bool {
    attempt.revision_request_id.is_some()
        && matches!(
            attempt.status,
            PlanReviewAttemptStatus::DraftReady
                | PlanReviewAttemptStatus::CompletedWithoutDraft
                | PlanReviewAttemptStatus::Cancelled
                | PlanReviewAttemptStatus::Interrupted
                | PlanReviewAttemptStatus::Blocked
                | PlanReviewAttemptStatus::Paused
                | PlanReviewAttemptStatus::Failed
        )
}

impl Session {
    /// Commits the revision's draft/base decision, actual attempt terminal and exact public
    /// notification at one existing writer-bundle boundary. No adapter owns a second outcome.
    pub fn append_plan_review_revision_terminal(
        &mut self,
        attempt: PlanReviewAttemptEntry,
        draft: Option<PlanDraftCreatedEntry>,
        decision: PlanDecisionRecordedEntry,
        event: PublicRunEvent,
    ) -> Result<PublicEventOutboxEntryV1> {
        validate_terminal_material(&attempt, draft.as_ref(), &decision, &event)?;
        if event.session_id != self.session_scope_id() {
            bail!("revision terminal belongs to another session");
        }
        let run_id = plan_review_revision_run_id(&attempt);
        let identity = format!("{}|{run_id}", event.session_id);
        let outbox = PublicEventOutboxEntryV1 {
            schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            domain_event_id: stable_event_uuid("sigil-plan-review-terminal-v1", &identity),
            public_event_id: stable_event_uuid("sigil-plan-review-terminal-public-v1", &identity),
            run_id: run_id.clone(),
            sequence: event.sequence,
            payload_digest: stable_event_hash(serde_json::to_vec(&event)?),
            event,
        };
        validate_public_event_outbox_entry(&outbox)?;
        let mut controls = Vec::new();
        if let Some(draft) = draft {
            controls.push(ControlEntry::PlanDraftCreated(draft));
        }
        controls.push(ControlEntry::PlanDecisionRecorded(decision));
        controls.push(ControlEntry::PlanReviewAttempt(attempt.clone()));
        for control in &controls {
            control.validate_durable_contract()?;
        }
        let store = self
            .durable_store()
            .context("revision terminal requires a durable session store")?;
        let mut events = controls
            .iter()
            .enumerate()
            .map(|(index, control)| {
                let entry = SessionLogEntry::Control(control.clone());
                let kind = store::session_entry_event_type(&entry);
                let id = if matches!(control, ControlEntry::PlanReviewAttempt(_)) {
                    outbox.domain_event_id.clone()
                } else {
                    stable_event_uuid(
                        "sigil-plan-review-terminal-control-v1",
                        &format!("{identity}|{index}"),
                    )
                };
                (
                    id,
                    kind,
                    store::session_entry_event_class(kind),
                    serde_json::json!({ "session_log_entry": entry }),
                )
            })
            .collect::<Vec<_>>();
        events.push((
            outbox.public_event_id.clone(),
            DurableEventType::PublicEventOutbox,
            EventClass::Critical,
            serde_json::to_value(&outbox)?,
        ));
        let expected = outbox.clone();
        let expected_controls = controls.clone();
        let decision_time = controls
            .iter()
            .find_map(|control| match control {
                ControlEntry::PlanDecisionRecorded(decision) => Some(decision.decided_at_ms),
                _ => None,
            })
            .expect("revision bundle always includes its base decision");
        store.append_crash_safe_events_if(events, move |records| {
            let projection = PublicEventOutboxProjectionV1::from_records(records)?;
            if let Some(existing) = projection.entry(&expected.public_event_id) {
                if serde_json::to_value(existing)? != serde_json::to_value(&expected)? {
                    bail!("revision terminal retry conflicts with the original public outcome");
                }
                let index = records
                    .iter()
                    .position(|record| record.stored_event().event_id == expected.domain_event_id)
                    .context("revision terminal is missing its domain envelope")?;
                let first = index
                    .checked_add(1)
                    .and_then(|end| end.checked_sub(expected_controls.len()))
                    .context("revision terminal controls are incomplete")?;
                for (record, control) in records[first..=index].iter().zip(&expected_controls) {
                    let Some(SessionLogEntry::Control(mut original)) =
                        record.session_log_entry()?
                    else {
                        bail!("revision terminal lost its original domain control");
                    };
                    // Timestamps are commit facts, not a new terminal intent on a retry.
                    match (&mut original, control) {
                        (
                            ControlEntry::PlanReviewAttempt(original),
                            ControlEntry::PlanReviewAttempt(retry),
                        ) => original.recorded_at_ms = retry.recorded_at_ms,
                        (
                            ControlEntry::PlanDecisionRecorded(original),
                            ControlEntry::PlanDecisionRecorded(retry),
                        ) => original.decided_at_ms = retry.decided_at_ms,
                        _ => {}
                    }
                    if serde_json::to_value(original)? != serde_json::to_value(control)? {
                        bail!("revision terminal retry conflicts with original domain facts");
                    }
                }
                return Ok(false);
            }
            if records
                .iter()
                .any(|record| record.session_id() != expected.event.session_id)
                || projection.events_in_order().iter().any(|entry| {
                    entry.run_id == expected.run_id && entry.sequence >= expected.sequence
                })
            {
                bail!("revision terminal session or public sequence is stale");
            }
            if decision_time != attempt.recorded_at_ms {
                bail!("a new revision bundle must share one commit timestamp");
            }
            validate_predecessor(&store::session_entries_from_records(records)?, &attempt)?;
            Ok(true)
        })?;
        self.reconcile_plan_review_revision_terminal(&run_id)?
            .context("committed revision terminal is missing its original outbox")
    }

    /// Recovers any pending writer intent, validates the exact terminal pair, and refreshes
    /// the live projection without replacing runtime attachments or artifact capabilities.
    /// The caller must own this session's foreground mutation boundary.
    pub fn reconcile_plan_review_revision_terminal(
        &mut self,
        run_id: &str,
    ) -> Result<Option<PublicEventOutboxEntryV1>> {
        let Some(store) = self.durable_store() else {
            return Ok(None);
        };
        let records = store.read_event_records_writer()?;
        let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
        let terminal = projection
            .events_in_order()
            .into_iter()
            .find(|entry| {
                entry.run_id == run_id
                    && records.iter().any(|record| {
                        record.stored_event().event_id == entry.domain_event_id
                            && record.stored_event().event_kind()
                                == Some(DurableEventType::PlanReviewAttempt)
                    })
            })
            .cloned();
        self.adopt_plan_review_recovery_records(&records)?;
        Ok(terminal)
    }

    /// Next persisted public sequence for a revision without an active live event bridge.
    /// Live bridges must instead reserve from their own single sequence owner.
    pub fn next_plan_review_public_sequence(&self, run_id: &str) -> Result<u64> {
        let Some(store) = self.durable_store() else {
            return Ok(1);
        };
        let records = store.read_event_records_writer()?;
        PublicEventOutboxProjectionV1::from_records(&records)?
            .events_in_order()
            .iter()
            .filter(|entry| entry.run_id == run_id)
            .map(|entry| entry.sequence)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .context("revision public sequence overflow")
    }
}

fn validate_predecessor(
    entries: &[SessionLogEntry],
    attempt: &PlanReviewAttemptEntry,
) -> Result<()> {
    let reviews = PlanReviewProjection::from_entries(entries);
    if reviews.has_conflicts() {
        bail!("revision predecessor has conflicting lifecycle facts");
    }
    let review = reviews
        .review(&attempt.plan_review_id)
        .context("revision has no durable start")?;
    if !review.attempts.iter().any(|entry| {
        entry.attempt_id == attempt.attempt_id && entry.status == PlanReviewAttemptStatus::Started
    }) {
        bail!("revision terminal requires a matching actual Started attempt");
    }
    let mut expected = review
        .latest_attempt()
        .context("revision has no current attempt")?
        .clone();
    expected.status = attempt.status;
    expected.terminal_reason = attempt.terminal_reason;
    expected.pending_user_input = None;
    expected.recorded_at_ms = attempt.recorded_at_ms;
    if &expected != attempt {
        bail!("revision terminal changes its immutable attempt binding");
    }
    reviews.validate_append(attempt)?;
    let artifacts = PlanArtifactProjection::from_entries(entries);
    let base = attempt
        .base_plan_id
        .as_ref()
        .context("revision has no base plan")?;
    let hash = attempt
        .base_plan_hash
        .as_ref()
        .context("revision has no base hash")?;
    if artifacts
        .plans
        .get(base)
        .is_none_or(|draft| &draft.plan_hash != hash)
        || artifacts.latest_decision(base).is_none_or(|decision| {
            decision.decision != PlanDecision::RevisionRequested || &decision.plan_hash != hash
        })
        || artifacts.plans.contains_key(&attempt.plan_id)
    {
        bail!("revision terminal requires exact pending base and no split candidate draft");
    }
    Ok(())
}

fn validate_terminal_material(
    attempt: &PlanReviewAttemptEntry,
    draft: Option<&PlanDraftCreatedEntry>,
    decision: &PlanDecisionRecordedEntry,
    event: &PublicRunEvent,
) -> Result<()> {
    if !is_revision_terminal(attempt)
        || attempt.recorded_at_ms == 0
        || event.schema_version != crate::PUBLIC_RUN_EVENT_SCHEMA_VERSION
        || event.run_id != plan_review_revision_run_id(attempt)
        || event.session_id != attempt.source_turn.session_scope_id
        || Some(&decision.plan_id) != attempt.base_plan_id.as_ref()
        || Some(&decision.plan_hash) != attempt.base_plan_hash.as_ref()
        || decision.decided_by != PlanDecisionActor::System
        || decision.decided_at_ms == 0
    {
        bail!("revision terminal has inconsistent identity or decision binding");
    }
    let success = attempt.status == PlanReviewAttemptStatus::DraftReady;
    if decision.decision
        != if success {
            PlanDecision::RevisionSucceeded
        } else {
            PlanDecision::RevisionFailed
        }
        || draft.is_some() != success
    {
        bail!("revision terminal does not bind its actual draft and base decision");
    }
    if let Some(draft) = draft
        && (draft.plan_id != attempt.plan_id
            || draft.source.source_turn.as_ref() != Some(&attempt.source_turn)
            || draft.source.plan_review_id.as_ref() != Some(&attempt.plan_review_id)
            || draft.source.route_decision_id != attempt.route_decision_id
            || draft.workspace_snapshot_id != attempt.workspace_snapshot_id)
    {
        bail!("revision draft has a different attempt lineage");
    }
    let matches = matches!(
        (attempt.status, &event.event),
        (
            PlanReviewAttemptStatus::DraftReady | PlanReviewAttemptStatus::CompletedWithoutDraft,
            PublicRunEventKind::RunFinished { .. }
        ) | (
            PlanReviewAttemptStatus::Cancelled,
            PublicRunEventKind::RunCancelled
        ) | (
            PlanReviewAttemptStatus::Interrupted,
            PublicRunEventKind::RunInterrupted { .. }
        ) | (
            PlanReviewAttemptStatus::Blocked,
            PublicRunEventKind::RunBlocked { .. }
        ) | (
            PlanReviewAttemptStatus::Paused,
            PublicRunEventKind::RunPaused { .. }
        ) | (
            PlanReviewAttemptStatus::Failed,
            PublicRunEventKind::RunFailed { .. }
        )
    );
    if !matches {
        bail!("revision terminal and public event disagree");
    }
    Ok(())
}

/// Checks both directions of every revision terminal pair and its contiguous domain bundle.
pub(super) fn validate_revision_pairs(
    records: &[SessionStreamRecord],
    projection: &PublicEventOutboxProjectionV1,
) -> Result<()> {
    let mut finalized = BTreeMap::new();
    let mut paired = std::collections::BTreeSet::new();
    for (index, record) in records.iter().enumerate() {
        let Some(SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))) =
            record.session_log_entry()?
        else {
            continue;
        };
        if !is_revision_terminal(&attempt) {
            continue;
        }
        let run_id = plan_review_revision_run_id(&attempt);
        if finalized.insert(run_id.clone(), ()).is_some() {
            bail!("revision has multiple terminal outcomes");
        }
        let entries = projection
            .events_in_order()
            .into_iter()
            .filter(|entry| entry.domain_event_id == record.stored_event().event_id)
            .collect::<Vec<_>>();
        let [outbox] = entries.as_slice() else {
            bail!("revision terminal has no unique original public pair");
        };
        paired.insert(outbox.public_event_id.as_str());
        let public = records
            .get(index + 1)
            .context("revision terminal lost its adjacent public envelope")?;
        if public.stored_event().event_id != outbox.public_event_id
            || public.stored_event().event_kind() != Some(DurableEventType::PublicEventOutbox)
            || record.stored_event().stream_sequence.checked_add(1)
                != Some(public.stored_event().stream_sequence)
            || record.session_id() != outbox.event.session_id
            || public.session_id() != outbox.event.session_id
        {
            bail!("revision terminal/outbox envelopes are not one exact pair");
        }
        let decision_index = index
            .checked_sub(1)
            .context("revision terminal lost its base decision")?;
        let Some(SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(decision))) =
            records[decision_index].session_log_entry()?
        else {
            bail!("revision terminal lost its adjacent base decision");
        };
        if decision.decided_at_ms != attempt.recorded_at_ms {
            bail!("revision bundle has inconsistent original commit timestamps");
        }
        let (first, draft) = if attempt.status == PlanReviewAttemptStatus::DraftReady {
            let first = decision_index
                .checked_sub(1)
                .context("revision success lost its draft")?;
            let Some(SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft))) =
                records[first].session_log_entry()?
            else {
                bail!("revision success lost its adjacent draft");
            };
            (first, Some(draft))
        } else {
            (decision_index, None)
        };
        validate_terminal_material(&attempt, draft.as_ref(), &decision, &outbox.event)?;
        validate_predecessor(
            &store::session_entries_from_records(&records[..first])?,
            &attempt,
        )?;
    }
    for entry in projection.events_in_order() {
        if records.iter().any(|record| {
            record.stored_event().event_id == entry.domain_event_id
                && record.stored_event().event_kind() == Some(DurableEventType::PlanReviewAttempt)
        }) && super::public_event_outbox::is_terminal_event(&entry.event.event)
            && !paired.contains(entry.public_event_id.as_str())
        {
            bail!("public revision terminal has no exact finalized attempt");
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/plan_review_terminal_tests.rs"]
mod tests;
