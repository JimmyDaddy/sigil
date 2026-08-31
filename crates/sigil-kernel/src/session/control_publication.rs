use super::writer::PendingStoredEvent;
use std::collections::BTreeSet;

use super::*;
use crate::{PublicRunEvent, PublicRunEventKind, PublicTaskEventProjector};

/// Rebuildable projection of this Session's append-only entries, never an authority or log.
#[derive(Debug, Clone, Default)]
pub(super) struct ControlPublicProjection {
    entry_count: usize,
    projector: PublicTaskEventProjector,
}

impl ControlPublicProjection {
    fn refresh(&mut self, entries: &[SessionLogEntry]) -> Result<()> {
        if self.entry_count > entries.len() {
            bail!("public control projection cannot rewind its session prefix");
        }
        for entry in &entries[self.entry_count..] {
            if let SessionLogEntry::Control(control) = entry {
                self.projector.project_control(control)?;
            }
            self.entry_count += 1;
        }
        Ok(())
    }
}

pub(super) fn project_explicit_control(
    projector: &mut PublicTaskEventProjector,
    control: &ControlEntry,
) -> Result<Vec<PublicRunEventKind>> {
    let events = projector.project_control(control)?;
    if !events.is_empty() {
        return Ok(events);
    }
    if matches!(control, ControlEntry::IntegrationLaneChanged(_)) {
        bail!("public integration lane projection requires its durable plan context");
    }
    let public = match control {
        // HTTP execution registry and Desktop tool lifecycle consume this typed control.
        ControlEntry::ToolExecution(_) => control.clone().into(),
        // Agent activity reload consumes only the kind, never private child references,
        // mailbox bodies, approval routing material or profile snapshots.
        ControlEntry::AgentProfileCaptured(_)
        | ControlEntry::AgentProfileTrustDecision(_)
        | ControlEntry::AgentProfilePolicyDecision(_)
        | ControlEntry::AgentDelegationAdmitted(_)
        | ControlEntry::AgentMailboxMessage(_)
        | ControlEntry::AgentResultContinuation(_)
        | ControlEntry::AgentThreadStarted(_)
        | ControlEntry::AgentThreadStatusChanged(_)
        | ControlEntry::AgentThreadMessageRouted(_)
        | ControlEntry::AgentThreadResultRecorded(_)
        | ControlEntry::AgentThreadResultDelivered(_)
        | ControlEntry::AgentThreadDisplayName(_)
        | ControlEntry::AgentApprovalRoute(_)
        | ControlEntry::AgentElicitationRoute(_)
        | ControlEntry::AgentUserInputRoute(_)
        | ControlEntry::AgentRunAttemptStarted(_)
        | ControlEntry::AgentRunHeartbeat(_)
        | ControlEntry::AgentRunInterrupted(_)
        | ControlEntry::AgentRouteClosed(_)
        | ControlEntry::AgentMergeSafePoint(_)
        | ControlEntry::AgentThreadClosed(_) => crate::PublicControlEvent {
            kind: crate::event::control_entry_kind(control).to_owned(),
            payload: None,
        },
        // No generic serde fallback: a new public consumer needs an explicit mapping.
        _ => return Ok(Vec::new()),
    };
    Ok(vec![PublicRunEventKind::Control { control: public }])
}

impl Session {
    /// Commits an explicitly observable control transition and its exact public DTOs in one
    /// existing writer append intent. Runtime proposes the next sequence; writer admission
    /// remains the sole durable frontier check. No adapter delivery occurs in this method.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid transition/projection, missing durable session, stale
    /// public frontier or an append whose original intent cannot be safely reconciled.
    pub fn append_controls_with_public_outbox(
        &mut self,
        controls: Vec<ControlEntry>,
        run_id: &str,
        next_sequence: u64,
    ) -> Result<(Vec<StoredEvent>, Vec<PublicEventOutboxEntryV1>)> {
        if controls.is_empty() || run_id.trim().is_empty() || next_sequence == 0 {
            bail!("public control transition requires controls and an active run sequence");
        }
        let store = self
            .store
            .as_ref()
            .context("public control transition requires a durable session")?;
        controls
            .iter()
            .try_for_each(ControlEntry::validate_durable_contract)?;
        self.validate_user_input_controls(controls.iter())?;
        self.validate_plan_controls(controls.iter())?;
        let mut candidate = self.control_public_projection.clone();
        candidate.refresh(&self.entries)?;
        let mut sequence = next_sequence;
        let mut pending = Vec::new();
        let mut outbox = Vec::new();
        for control in &controls {
            // A private-only transition does not consume a public sequence. Allocate source
            // identity once per invocation, then freeze it in the same recoverable intent.
            let domain_id = uuid::Uuid::new_v4().to_string();
            let entry = SessionLogEntry::Control(control.clone());
            let event_type = super::store::session_entry_event_type(&entry);
            if event_type == DurableEventType::ConversationInputPromoted {
                bail!("conversation input promotion requires its dedicated critical append API");
            }
            pending.push(PendingStoredEvent {
                event_type,
                event_class: super::store::session_entry_event_class(event_type),
                payload: serde_json::json!({ "session_log_entry": entry }),
                event_id: Some(domain_id.clone()),
                correlation_id: Some(domain_id.clone()),
                causation_id: None,
            });
            for event in project_explicit_control(&mut candidate.projector, control)? {
                let public =
                    PublicRunEvent::new(self.session_scope_id.clone(), run_id, sequence, event);
                let public_event_id = format!(
                    "application-public:{}:{run_id}:{sequence}",
                    self.session_scope_id
                );
                let entry = PublicEventOutboxEntryV1 {
                    schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
                    domain_event_id: domain_id.clone(),
                    public_event_id: public_event_id.clone(),
                    run_id: run_id.to_owned(),
                    sequence,
                    payload_digest: stable_event_hash(&serde_json::to_vec(&public)?),
                    event: public,
                };
                if super::public_event_outbox::is_terminal_event(&entry.event.event) {
                    bail!("control publication cannot create a run terminal");
                }
                pending.push(PendingStoredEvent {
                    event_type: DurableEventType::PublicEventOutbox,
                    event_class: EventClass::Critical,
                    payload: serde_json::to_value(&entry)?,
                    event_id: Some(public_event_id.clone()),
                    correlation_id: Some(domain_id.clone()),
                    causation_id: Some(domain_id.clone()),
                });
                outbox.push(entry);
                sequence = sequence
                    .checked_add(1)
                    .context("public control sequence exhausted")?;
            }
        }
        let events = store.append_control_publication(pending, run_id)?;
        let domain_events = events
            .into_iter()
            .filter(|event| event.event_kind() != Some(DurableEventType::PublicEventOutbox))
            .collect::<Vec<_>>();
        self.entries
            .extend(controls.into_iter().map(SessionLogEntry::Control));
        candidate.entry_count = self.entries.len();
        self.control_public_projection = candidate;
        self.advance_durable_session_entry_count(&domain_events);
        Ok((domain_events, outbox))
    }
}

/// Validates the original source-to-DTO mapping on replay without creating missing events.
/// Old controls without publications remain readable; a linked publication must be complete,
/// adjacent to its source, and equal to the typed projection at that exact durable prefix.
pub(super) fn validate_control_publication_pairs(
    records: &[SessionStreamRecord],
    outbox: &PublicEventOutboxProjectionV1,
) -> Result<()> {
    let mut linked = BTreeMap::<&str, Vec<&PublicEventOutboxEntryV1>>::new();
    for entry in outbox.events_in_order() {
        if entry.domain_event_id != entry.public_event_id
            && !super::public_event_outbox::is_terminal_event(&entry.event.event)
        {
            linked
                .entry(&entry.domain_event_id)
                .or_default()
                .push(entry);
        }
    }
    if linked.is_empty() {
        return Ok(());
    }
    let mut active_run_ids = BTreeSet::new();
    let mut projector = PublicTaskEventProjector::default();
    for (index, record) in records.iter().enumerate() {
        super::public_event_outbox::apply_public_run_admission_record(&mut active_run_ids, record)?;
        let source = record.stored_event();
        let Some(SessionLogEntry::Control(control)) = record.session_log_entry()? else {
            continue;
        };
        let Some(publications) = linked.remove(source.event_id.as_str()) else {
            // A domain-only record may carry context without promising a public DTO. Preserve
            // that distinction when rebuilding old/private prefixes; only linked sources must
            // satisfy the stricter explicit-publication projection contract.
            projector.project_control(&control)?;
            continue;
        };
        let expected = project_explicit_control(&mut projector, &control)?;
        if source.event_kind()
            != Some(super::store::session_entry_event_type(
                &SessionLogEntry::Control(control.clone()),
            ))
        {
            bail!("public control source uses an incorrect durable event type");
        }
        if publications.len() != expected.len() {
            bail!("public control publication is missing part of its source projection");
        }
        let run_id = &publications[0].run_id;
        if !active_run_ids.contains(run_id) {
            bail!("public control publication source is outside its active durable run");
        }
        for (ordinal, (entry, expected)) in publications.iter().zip(expected).enumerate() {
            let envelope = records
                .get(index + ordinal + 1)
                .context("public control publication is detached from its source")?
                .stored_event();
            if envelope.event_id != entry.public_event_id
                || envelope.causation_id.as_deref() != Some(source.event_id.as_str())
                || envelope.correlation_id != source.correlation_id
                || entry.event.session_id != source.session_id
                || entry.run_id != *run_id
                || serde_json::to_value(&entry.event.event)? != serde_json::to_value(expected)?
            {
                bail!("public control publication does not match its exact source projection");
            }
        }
    }
    if !linked.is_empty() {
        bail!("public control publication has no durable source control");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/control_publication_tests.rs"]
mod tests;
