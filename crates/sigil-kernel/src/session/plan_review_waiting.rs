//! A resumable revision suspension and its notification share one durable boundary.

use std::collections::BTreeSet;

use super::*;
use crate::{PlanReviewAttemptEntry, PlanReviewAttemptStatus, PublicRunEvent, PublicRunEventKind};

pub(super) fn is_revision_waiting(attempt: &PlanReviewAttemptEntry) -> bool {
    attempt.revision_request_id.is_some()
        && attempt.status == PlanReviewAttemptStatus::WaitingForInput
}

impl Session {
    /// Commits one exact revision input suspension and its public event atomically.
    /// The child retains input/continuation authority; this parent record exposes its waiting
    /// state without finalizing the attempt or settling the base plan.
    ///
    /// # Errors
    /// Returns an error for stale lineage/sequence, a different request, or unconfirmed storage.
    pub fn append_plan_review_revision_waiting(
        &mut self,
        attempt: PlanReviewAttemptEntry,
        event: PublicRunEvent,
    ) -> Result<PublicEventOutboxEntryV1> {
        validate_waiting_material(&attempt, &event)?;
        if event.session_id != self.session_scope_id() {
            bail!("revision suspension belongs to another session");
        }
        let pending = attempt
            .pending_user_input
            .as_ref()
            .expect("validated pending input");
        let identity = format!(
            "{}|{}|{}|{}|{}",
            event.session_id,
            event.run_id,
            pending.identity.request_id.as_str(),
            pending.identity.generation,
            pending.request_hash,
        );
        let outbox = PublicEventOutboxEntryV1 {
            schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: stable_event_uuid("sigil-plan-review-waiting-public-v1", &identity),
            domain_event_id: stable_event_uuid("sigil-plan-review-waiting-v1", &identity),
            run_id: event.run_id.clone(),
            sequence: event.sequence,
            payload_digest: stable_event_hash(serde_json::to_vec(&event)?),
            event,
        };
        validate_public_event_outbox_entry(&outbox)?;
        let control = ControlEntry::PlanReviewAttempt(attempt.clone());
        control.validate_durable_contract()?;
        let store = self
            .durable_store()
            .context("revision suspension requires a durable store")?;
        let events = vec![
            (
                outbox.domain_event_id.clone(),
                DurableEventType::PlanReviewAttempt,
                EventClass::Critical,
                serde_json::json!({"session_log_entry": SessionLogEntry::Control(control)}),
            ),
            (
                outbox.public_event_id.clone(),
                DurableEventType::PublicEventOutbox,
                EventClass::Critical,
                serde_json::to_value(&outbox)?,
            ),
        ];
        let expected = outbox.clone();
        store.append_crash_safe_events_if(events, move |records| {
            let projection = PublicEventOutboxProjectionV1::from_records(records)?;
            if let Some(existing) = projection.entry(&expected.public_event_id) {
                if serde_json::to_value(existing)? != serde_json::to_value(&expected)? {
                    bail!("revision suspension retry conflicts with its original public event");
                }
                let original = records
                    .iter()
                    .find(|record| record.stored_event().event_id == expected.domain_event_id)
                    .context("revision suspension lost its original attempt")?;
                let Some(SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(mut original))) =
                    original.session_log_entry()?
                else {
                    bail!("revision suspension has no original domain record");
                };
                original.recorded_at_ms = attempt.recorded_at_ms;
                if original != attempt {
                    bail!("revision suspension retry changes original domain facts");
                }
                let reviews = crate::PlanReviewProjection::from_entries(
                    &store::session_entries_from_records(records)?,
                );
                let mut latest = reviews
                    .latest_attempt(&attempt.plan_review_id)
                    .context("revision suspension has no current attempt")?
                    .clone();
                latest.recorded_at_ms = attempt.recorded_at_ms;
                if latest != attempt {
                    bail!("revision suspension retry is no longer current");
                }
                return Ok(false);
            }
            if records
                .iter()
                .any(|record| record.session_id() != expected.event.session_id)
            {
                bail!("revision suspension has a different session identity");
            }
            let next = projection
                .events_in_order()
                .iter()
                .filter(|entry| entry.run_id == expected.run_id)
                .map(|entry| entry.sequence)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .context("revision suspension public sequence exhausted")?;
            if next != expected.sequence {
                bail!("revision suspension public sequence is stale");
            }
            super::plan_review_terminal::validate_predecessor(
                &store::session_entries_from_records(records)?,
                &attempt,
            )?;
            Ok(true)
        })?;
        let records = store.read_event_records_writer()?;
        let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
        self.adopt_plan_review_recovery_records(&records)?;
        projection
            .entry(&outbox.public_event_id)
            .cloned()
            .context("committed revision suspension has no original public event")
    }

    /// Recovers the original public event only while this exact revision remains suspended.
    /// Replaying history is separate; answered or finalized attempts are not reopened here.
    ///
    /// # Errors
    /// Returns an error for an incomplete/corrupt pair or unavailable writer recovery.
    pub fn reconcile_plan_review_revision_waiting(
        &mut self,
        run_id: &str,
    ) -> Result<Option<PublicEventOutboxEntryV1>> {
        let Some(store) = self.durable_store() else {
            return Ok(None);
        };
        let records = store.read_event_records_writer()?;
        let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
        let mut current = None;
        for record in &records {
            if let Some(SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))) =
                record.session_log_entry()?
                && super::plan_review_terminal::plan_review_revision_run_id(&attempt) == run_id
            {
                current =
                    is_revision_waiting(&attempt).then_some(record.stored_event().event_id.clone());
            }
        }
        let waiting = current.and_then(|domain_id| {
            projection
                .events_in_order()
                .into_iter()
                .find(|entry| entry.domain_event_id == domain_id)
                .cloned()
        });
        self.adopt_plan_review_recovery_records(&records)?;
        Ok(waiting)
    }
}

fn validate_waiting_material(
    attempt: &PlanReviewAttemptEntry,
    event: &PublicRunEvent,
) -> Result<()> {
    if !is_revision_waiting(attempt)
        || attempt.recorded_at_ms == 0
        || attempt.terminal_reason.is_some()
        || event.schema_version != crate::PUBLIC_RUN_EVENT_SCHEMA_VERSION
        || event.run_id != super::plan_review_terminal::plan_review_revision_run_id(attempt)
        || event.session_id != attempt.source_turn.session_scope_id
    {
        bail!("revision suspension has inconsistent identity or status");
    }
    crate::conversation_route::validate_attempt_payload(attempt)?;
    let pending = attempt
        .pending_user_input
        .as_ref()
        .context("revision suspension lost its request")?;
    if pending.identity.root_logical_run_id.as_str() != event.run_id {
        bail!("revision suspension request belongs to another logical run");
    }
    if !matches!(&event.event, PublicRunEventKind::RunAwaitingUserInput {
        request_id, generation, request_hash,
    } if request_id == pending.identity.request_id.as_str()
        && *generation == pending.identity.generation
        && request_hash == &pending.request_hash)
    {
        bail!("revision suspension public event changes its pending request");
    }
    Ok(())
}

pub(super) fn validate_waiting_pairs(
    records: &[SessionStreamRecord],
    projection: &PublicEventOutboxProjectionV1,
) -> Result<()> {
    let mut paired = BTreeSet::new();
    for (index, record) in records.iter().enumerate() {
        let Some(SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))) =
            record.session_log_entry()?
        else {
            continue;
        };
        if !is_revision_waiting(&attempt) {
            continue;
        }
        let entries = projection
            .events_in_order()
            .into_iter()
            .filter(|entry| entry.domain_event_id == record.stored_event().event_id)
            .collect::<Vec<_>>();
        let [entry] = entries.as_slice() else {
            bail!("revision suspension has no unique original public pair");
        };
        let public = records
            .get(index + 1)
            .context("revision suspension lost its adjacent public envelope")?;
        if public.stored_event().event_kind() != Some(DurableEventType::PublicEventOutbox)
            || public.stored_event().event_id != entry.public_event_id
            || public.session_id() != entry.event.session_id
            || record.stored_event().stream_sequence.checked_add(1)
                != Some(public.stored_event().stream_sequence)
        {
            bail!("revision suspension/outbox envelopes are not one exact pair");
        }
        validate_waiting_material(&attempt, &entry.event)?;
        super::plan_review_terminal::validate_predecessor(
            &store::session_entries_from_records(&records[..index])?,
            &attempt,
        )?;
        paired.insert(entry.public_event_id.as_str());
    }
    for entry in projection.events_in_order() {
        if matches!(
            entry.event.event,
            PublicRunEventKind::RunAwaitingUserInput { .. }
        ) && records.iter().any(|record| {
            record.stored_event().event_id == entry.domain_event_id
                && record.stored_event().event_kind() == Some(DurableEventType::PlanReviewAttempt)
        }) && !paired.contains(entry.public_event_id.as_str())
        {
            bail!("public revision suspension has no exact waiting attempt");
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/plan_review_waiting_tests.rs"]
mod tests;
