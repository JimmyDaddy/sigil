use super::writer::PendingStoredEvent;
use super::*;
use std::collections::BTreeSet;

const DECLINED_REASON: &str = "The user declined to answer the pending request.";

pub(super) fn terminal_event(
    resolution: &crate::UserInputResolutionV1,
) -> Option<crate::PublicRunEventKind> {
    match resolution {
        crate::UserInputResolutionV1::RunCancelled => Some(crate::PublicRunEventKind::RunCancelled),
        crate::UserInputResolutionV1::Declined => Some(crate::PublicRunEventKind::RunBlocked {
            reason: DECLINED_REASON.to_owned(),
        }),
        _ => None,
    }
}

pub(super) fn terminal_event_matches(
    resolution: &crate::UserInputResolutionV1,
    event: &crate::PublicRunEventKind,
) -> bool {
    matches!(
        (resolution, event),
        (
            crate::UserInputResolutionV1::RunCancelled,
            crate::PublicRunEventKind::RunCancelled
        )
    ) || matches!((resolution, event), (crate::UserInputResolutionV1::Declined, crate::PublicRunEventKind::RunBlocked { reason }) if reason == DECLINED_REASON)
}

/// A declined or cancelled input succeeds its exact Awaiting terminal without finalizing twice.
pub(super) fn awaiting_input_run(
    records: &[SessionStreamRecord],
    resolved: &crate::UserInputResolvedV1,
) -> Result<Option<String>> {
    if terminal_event(&resolved.resolution).is_none() {
        return Ok(None);
    }
    let entries = records
        .iter()
        .map(SessionStreamRecord::session_log_entry)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let projection = crate::UserInputProjectionV1::from_session_entries(&entries)?;
    let Some(state) = projection.request(&resolved.identity) else {
        return Ok(None);
    };
    if state.requested.request_hash != resolved.request_hash
        || !matches!(
            state.requested.request.source,
            crate::UserInputSourceV1::Agent
        )
    {
        return Ok(None);
    }
    let mut terminal_runs = BTreeSet::new();
    let mut awaiting = None;
    let mut conflicting_request = false;
    for record in records.iter().rev() {
        if record.stored_event().event_kind() != Some(DurableEventType::PublicEventOutbox) {
            continue;
        }
        let outbox: PublicEventOutboxEntryV1 =
            serde_json::from_value(record.stored_event().payload.clone())?;
        if !super::public_event_outbox::is_terminal_event(&outbox.event.event)
            || !terminal_runs.insert(outbox.run_id.clone())
        {
            continue;
        }
        let crate::PublicRunEventKind::RunAwaitingUserInput {
            request_id,
            generation,
            request_hash,
        } = &outbox.event.event
        else {
            continue;
        };
        if request_id != resolved.identity.request_id.as_str() {
            continue;
        }
        if *generation != resolved.identity.generation
            || request_hash != &resolved.request_hash
            || outbox.event.session_id != resolved.identity.session_scope_id.as_str()
        {
            conflicting_request = true;
            continue;
        }
        if awaiting.replace(outbox.run_id).is_some() {
            bail!("terminal input has multiple exact awaiting runs");
        }
    }
    if awaiting.is_none() && conflicting_request {
        bail!("terminal input decision conflicts with its original awaiting binding");
    }
    Ok(awaiting)
}

pub(super) fn terminal_source(
    records: &[SessionStreamRecord],
    outbox: &PublicEventOutboxEntryV1,
) -> Result<Option<crate::UserInputResolvedV1>> {
    if !matches!(
        outbox.event.event,
        crate::PublicRunEventKind::RunCancelled | crate::PublicRunEventKind::RunBlocked { .. }
    ) {
        return Ok(None);
    }
    let Some(index) = records
        .iter()
        .position(|record| record.event_id() == outbox.domain_event_id)
    else {
        return Ok(None);
    };
    let Some(SessionLogEntry::Control(ControlEntry::UserInputResolved(resolved))) =
        records[index].session_log_entry()?
    else {
        return Ok(None);
    };
    if !terminal_event_matches(&resolved.resolution, &outbox.event.event)
        || outbox.event.session_id != resolved.identity.session_scope_id.as_str()
        || awaiting_input_run(&records[..index], &resolved)?.as_deref()
            != Some(outbox.run_id.as_str())
    {
        bail!("terminal input publication does not bind its exact awaiting root");
    }
    let entries = records[..index]
        .iter()
        .map(SessionStreamRecord::session_log_entry)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let projection = crate::UserInputProjectionV1::from_session_entries(&entries)?;
    let state = projection
        .request(&resolved.identity)
        .context("terminal input source is missing")?;
    if state.requested.request_hash != resolved.request_hash
        || !state.decision.as_ref().is_some_and(|accepted| {
            matches!(
                (&resolved.resolution, &accepted.decision),
                (
                    crate::UserInputResolutionV1::RunCancelled,
                    crate::UserInputDurableDecisionV1::RunCancelled
                ) | (
                    crate::UserInputResolutionV1::Declined,
                    crate::UserInputDurableDecisionV1::Declined
                )
            )
        })
    {
        bail!("terminal input publication has no exact accepted terminal decision");
    }
    let binding = state
        .requested
        .request
        .continuation
        .as_ref()
        .context("terminal input has no tool settlement binding")?;
    let result = index
        .checked_sub(2)
        .and_then(|index| records.get(index))
        .map(SessionStreamRecord::session_log_entry)
        .transpose()?
        .flatten();
    let execution = index
        .checked_sub(1)
        .and_then(|index| records.get(index))
        .map(SessionStreamRecord::session_log_entry)
        .transpose()?
        .flatten();
    if !matches!(result, Some(SessionLogEntry::ToolResultV3(ref result))
        if result.call_id == binding.tool_call_id && result.tool_name == crate::REQUEST_USER_INPUT_TOOL_NAME)
        || !matches!(execution, Some(SessionLogEntry::Control(ControlEntry::ToolExecution(ref execution)))
            if execution.call_id == binding.tool_call_id
                && execution.tool_name == crate::REQUEST_USER_INPUT_TOOL_NAME
                && execution.status == crate::ToolExecutionStatus::Completed)
    {
        bail!("terminal input publication has no adjacent exact tool settlement");
    }
    Ok(Some(resolved))
}

impl Session {
    /// Returns the exact public successor committed with an ordinary decline or cancellation.
    /// Historical private-only resolutions do not manufacture missing public events.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid durable source/publication bindings or a scope mismatch.
    pub fn user_input_terminal_outbox(
        &self,
        identity: &crate::UserInputIdentityV1,
        request_hash: &str,
    ) -> Result<Option<PublicEventOutboxEntryV1>> {
        if identity.session_scope_id.as_str() != self.session_scope_id() {
            bail!("terminal input belongs to another session");
        }
        let records = self
            .store
            .as_ref()
            .context("terminal input requires a durable session")?
            .read_current_event_records_writer()?;
        let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
        for outbox in projection.events_in_order() {
            if !matches!(
                outbox.event.event,
                crate::PublicRunEventKind::RunCancelled
                    | crate::PublicRunEventKind::RunBlocked { .. }
            ) {
                continue;
            }
            if let Some(resolved) = terminal_source(&records, outbox)?
                && &resolved.identity == identity
                && resolved.request_hash == request_hash
            {
                return Ok(Some(outbox.clone()));
            }
        }
        Ok(None)
    }

    pub(super) fn append_user_input_terminal_bundle(
        &mut self,
        entries: &[SessionLogEntry],
    ) -> Result<bool> {
        let Some(resolved) = entries.iter().find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::UserInputResolved(resolved))
                if terminal_event(&resolved.resolution).is_some() =>
            {
                Some(resolved)
            }
            _ => None,
        }) else {
            return Ok(false);
        };
        let Some(store) = self.store.as_ref() else {
            return Ok(false);
        };
        let records = store.read_current_event_records_writer()?;
        let Some(run_id) = awaiting_input_run(&records, resolved)? else {
            return Ok(false);
        };
        // Publish the resolved terminal only after its exact internal tool has settled. These
        // records, the public successor and any operation marker still share one crash-safe append.
        let entries = entries
            .iter()
            .filter(|entry| {
                !matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::UserInputResolved(_))
                )
            })
            .chain(entries.iter().filter(|entry| {
                matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::UserInputResolved(_))
                )
            }))
            .cloned()
            .collect::<Vec<_>>();
        let run_id = run_id.as_str();
        let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
        let sequence = projection
            .events_in_order()
            .iter()
            .filter(|entry| entry.run_id == run_id)
            .map(|entry| entry.sequence)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .context("terminal input public sequence exhausted")?;
        let mut pending = Vec::with_capacity(entries.len() + 2);
        for entry in &entries {
            let event_type = super::store::session_entry_event_type(entry);
            let domain_id = uuid::Uuid::new_v4().to_string();
            pending.push(PendingStoredEvent {
                event_type,
                event_class: super::store::session_entry_event_class(event_type),
                payload: serde_json::json!({"session_log_entry": entry}),
                event_id: Some(domain_id.clone()),
                correlation_id: None,
                causation_id: None,
            });
            if matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::UserInputResolved(_))
            ) {
                let public = crate::PublicRunEvent::new(
                    self.session_scope_id(),
                    run_id,
                    sequence,
                    terminal_event(&resolved.resolution)
                        .context("input has no terminal public outcome")?,
                );
                let outbox = PublicEventOutboxEntryV1 {
                    schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
                    public_event_id: format!(
                        "application-public:{}:{run_id}:{sequence}",
                        self.session_scope_id()
                    ),
                    domain_event_id: domain_id,
                    run_id: run_id.to_owned(),
                    sequence,
                    payload_digest: crate::stable_event_hash(&serde_json::to_vec(&public)?),
                    event: public,
                };
                pending.push(PendingStoredEvent {
                    event_type: DurableEventType::PublicEventOutbox,
                    event_class: EventClass::Critical,
                    payload: serde_json::to_value(&outbox)?,
                    event_id: Some(outbox.public_event_id),
                    correlation_id: None,
                    causation_id: None,
                });
            }
        }
        let operation = self.runtime_attachments.application_operation.clone().filter(|binding|
            entries.iter().any(|entry| matches!(entry, SessionLogEntry::Control(control) if binding.target.matches(control))));
        let marker = operation
            .as_ref()
            .map(|binding| {
                super::application_operation::add_operation_marker(binding, &mut pending)
            })
            .transpose()?;
        let events =
            store.append_user_input_terminal(pending, run_id, resolved, operation.as_ref())?;
        let domain_events = events
            .into_iter()
            .filter(|event| event.event_kind() != Some(DurableEventType::PublicEventOutbox))
            .collect::<Vec<_>>();
        self.bind_tool_artifacts_after_append(&entries, &domain_events);
        self.entries.extend_from_slice(&entries);
        if let Some(marker) = marker {
            self.entries.push(SessionLogEntry::Control(marker));
            self.runtime_attachments.application_operation = None;
        }
        self.advance_durable_session_entry_count(&domain_events);
        Ok(true)
    }
}
