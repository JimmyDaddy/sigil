//! Operation preparation and domain publication use the session's existing writer capability.

use std::collections::VecDeque;

use super::writer::PendingStoredEvent;
use super::*;
use crate::{
    ApplicationOperationBindingV1, ApplicationOperationCommittedV1, ApplicationOperationEvidenceV1,
};

#[cfg(test)]
#[path = "tests/application_operation_tests.rs"]
mod tests;

#[path = "application_operation_resolution.rs"]
mod resolution;

#[derive(Debug, Clone)]
pub struct SessionApplicationOperationOwner {
    store: JsonlSessionStore,
    session_scope_id: String,
    authority_scope_id: String,
}

impl SessionApplicationOperationOwner {
    pub fn append_queue_mutation(
        &self,
        binding: &ApplicationOperationBindingV1,
        command: crate::ConversationQueueMutationCommand,
    ) -> Result<crate::ConversationQueueMutationReceipt> {
        if binding.session_scope_id != self.session_scope_id {
            bail!("application queue operation scope mismatch");
        }
        if !validate_prepared(&self.store.read_current_event_records_writer()?, binding)? {
            bail!("application queue operation is not prepared");
        }
        self.store
            .append_conversation_queue_mutation_bound(command, Some(binding))
    }
    /// Reattaches local control through this capability, including while a run owns another
    /// in-memory Session. It does not discover a writer from a path.
    pub fn attach_for_control(&self) -> Result<Session> {
        Session::load_from_store_for_control(self.store.clone())
    }
    /// Strict observation through this existing owner; does not repair tails or append recovery.
    pub fn attach_for_observation(&self) -> Result<Session> {
        Session::observe_existing_store(self.store.clone(), &self.session_scope_id)
    }
    pub fn read_handle(&self) -> SessionRecordReadHandle {
        self.store.read_handle()
    }

    /// Allocates the operation durably before the command crosses the worker boundary.
    pub fn prepare(&self, binding: &ApplicationOperationBindingV1) -> Result<()> {
        let (owner, binding) = self.resolve_operation(binding)?;
        owner.prepare_local(&binding)
    }

    fn prepare_local(&self, binding: &ApplicationOperationBindingV1) -> Result<()> {
        binding.validate()?;
        if binding.domain_session_scope_id() != self.session_scope_id
            || binding.session_scope_id != self.authority_scope_id
        {
            bail!("application operation owner scope mismatch");
        }
        let entry = SessionLogEntry::Control(ControlEntry::ApplicationOperationPreparedV1(
            binding.clone(),
        ));
        let expected = binding.clone();
        self.store.append_crash_safe_events_if(
            vec![(
                prepared_id(binding),
                DurableEventType::ApplicationOperationPreparedV1,
                EventClass::Critical,
                serde_json::json!({"session_log_entry":entry}),
            )],
            move |records| {
                validate_record_scope(records, expected.domain_session_scope_id())?;
                Ok(!validate_prepared(records, &expected)?)
            },
        )?;
        Ok(())
    }
}

fn prepared_id(binding: &ApplicationOperationBindingV1) -> String {
    stable_event_uuid(
        "sigil-application-operation-prepared-v1",
        &binding.operation_id,
    )
}

fn domain_id(binding: &ApplicationOperationBindingV1, index: usize) -> String {
    stable_event_uuid(
        "sigil-application-operation-domain-v1",
        &format!("{}:{index}", binding.operation_id),
    )
}

const MAX_OPERATION_BATCH_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn add_operation_marker(
    binding: &ApplicationOperationBindingV1,
    pending: &mut Vec<PendingStoredEvent>,
) -> Result<ControlEntry> {
    binding.validate()?;
    if pending.is_empty() || pending.len() > 1024 {
        bail!("application operation batch exceeds its bound");
    }
    let mut matched = false;
    let mut bytes = 0;
    let mut evidence = Vec::new();
    for (index, event) in pending.iter_mut().enumerate() {
        if let Some(value) = event.payload.get("session_log_entry")
            && let SessionLogEntry::Control(control) = serde_json::from_value(value.clone())?
        {
            matched |= binding.target.matches(&control);
        }
        let payload = crate::event::canonical_json_bytes(&event.payload)?;
        bytes += payload.len();
        if bytes > MAX_OPERATION_BATCH_BYTES {
            bail!("application operation batch exceeds its byte bound");
        }
        let event_id = event
            .event_id
            .get_or_insert_with(|| domain_id(binding, index))
            .clone();
        evidence.push(ApplicationOperationEvidenceV1 {
            event_id,
            payload_digest: crate::sha256_hex(&payload),
        });
    }
    if !matched {
        bail!("application operation batch does not match its bound domain target");
    }
    let commit = ApplicationOperationCommittedV1 {
        binding: binding.clone(),
        domain_events: evidence,
    };
    commit.validate()?;
    let marker = ControlEntry::ApplicationOperationCommittedV1(commit);
    // Generic event causation must use the predecessor's existing correlation chain. Prepared
    // has no generic correlation when emitted by the tuple API; its operation identity is
    // already bound by the marker payload. Keep the actual domain batch's correlation intact.
    let predecessor = pending
        .last()
        .context("operation batch has no predecessor")?;
    let correlation_id = predecessor.correlation_id.clone();
    let causation_id = correlation_id.as_ref().and(predecessor.event_id.clone());
    pending.push(PendingStoredEvent {
        event_id: Some(stable_event_uuid(
            "sigil-application-operation-committed-v1",
            &binding.operation_id,
        )),
        event_type: DurableEventType::ApplicationOperationCommittedV1,
        event_class: EventClass::Critical,
        payload: serde_json::json!({"session_log_entry":SessionLogEntry::Control(marker.clone())}),
        correlation_id,
        causation_id,
    });
    Ok(marker)
}

fn validate_record_scope(records: &[SessionStreamRecord], scope: &str) -> Result<()> {
    if records.is_empty() || records.iter().any(|record| record.session_id() != scope) {
        bail!("application operation requires its existing owned session");
    }
    Ok(())
}

fn validate_prepared(
    records: &[SessionStreamRecord],
    binding: &ApplicationOperationBindingV1,
) -> Result<bool> {
    let mut found = false;
    for record in records {
        if let Some(SessionLogEntry::Control(ControlEntry::ApplicationOperationPreparedV1(prior))) =
            record.session_log_entry()?
            && prior.reservation_key_digest == binding.reservation_key_digest
        {
            if &prior != binding {
                bail!("application operation changes original K/F or target");
            }
            found = true;
        }
    }
    Ok(found)
}

impl Session {
    /// Narrows this existing attachment's writer to operation preparation and owned reads.
    pub fn application_operation_owner(&self) -> Result<SessionApplicationOperationOwner> {
        Ok(SessionApplicationOperationOwner {
            store: self
                .durable_store()
                .context("application operation requires durable Session owner")?,
            session_scope_id: self.session_scope_id().to_owned(),
            authority_scope_id: self.session_scope_id().to_owned(),
        })
    }

    /// Binds only the next matching domain transition, retaining its existing validation path.
    pub fn bind_application_operation(
        &mut self,
        binding: ApplicationOperationBindingV1,
    ) -> Result<()> {
        binding.validate()?;
        let owner = self.application_operation_owner()?;
        let (owner, binding) = if self.session_scope_id() == binding.session_scope_id {
            owner.resolve_operation(&binding)?
        } else {
            if self.session_scope_id() != binding.domain_session_scope_id() {
                bail!("application operation attachment scope mismatch");
            }
            (owner, binding)
        };
        if !validate_prepared(&owner.store.read_current_event_records_writer()?, &binding)? {
            bail!("application operation was not prepared by its owner");
        }
        if self
            .runtime_attachments
            .application_operation
            .as_ref()
            .is_some_and(|current| current != &binding)
        {
            bail!("another application operation owns this transition");
        }
        self.runtime_attachments.application_operation = Some(binding);
        Ok(())
    }

    /// Checks a domain transition against the caller's already-prepared command, when present.
    /// Unbound domain callers retain their normal validation and writer path.
    ///
    /// # Errors
    /// Rejects a transition that differs from the existing exact application operation target.
    pub fn ensure_application_operation_target(&self, control: &ControlEntry) -> Result<()> {
        if self
            .runtime_attachments
            .application_operation
            .as_ref()
            .is_some_and(|binding| !binding.target.matches(control))
        {
            bail!("domain transition differs from its prepared application operation");
        }
        Ok(())
    }

    /// Validates a multi-stage operation before publishing its precursor controls.
    ///
    /// # Errors
    /// Rejects a different target without treating the precursor as a completed operation.
    pub fn ensure_application_operation_binding_target(
        &self,
        target: &crate::ApplicationOperationTargetV1,
    ) -> Result<()> {
        if self
            .runtime_attachments
            .application_operation
            .as_ref()
            .is_some_and(|binding| &binding.target != target)
        {
            bail!("domain transition differs from its prepared application operation");
        }
        Ok(())
    }

    /// Commits this bound command's acceptance by an already-started foreground run.
    /// Unbound runs retain their existing lifecycle without inventing application evidence.
    ///
    /// # Errors
    /// Rejects a missing/conflicting durable run or an uncertain marker publication.
    pub fn record_bound_conversation_run_admission(&mut self, run_id: &str) -> Result<()> {
        let Some(binding) = self.runtime_attachments.application_operation.as_ref() else {
            return Ok(());
        };
        let crate::ApplicationOperationTargetV1::ConversationRunAdmission { input_digest } =
            &binding.target
        else {
            return Ok(());
        };
        self.append_control(ControlEntry::ConversationRunAcceptedV1(
            crate::ConversationRunAcceptedV1 {
                schema_version: 1,
                run_id: run_id.to_owned(),
                input_digest: input_digest.clone(),
            },
        ))
    }

    /// Detaches the process-local binding without changing its durable disposition.
    pub fn clear_application_operation(&mut self) -> Option<ApplicationOperationBindingV1> {
        self.runtime_attachments.application_operation.take()
    }

    /// Returns the identity of the already-prepared operation attached to this writer.
    /// This is a read-only identity projection, not an admission or a new authority.
    pub fn application_operation_id(&self) -> Option<&str> {
        self.runtime_attachments
            .application_operation
            .as_ref()
            .map(|binding| binding.operation_id.as_str())
    }

    pub fn has_application_operation(&self) -> bool {
        self.runtime_attachments.application_operation.is_some()
    }

    pub(super) fn append_bound_application_controls(
        &mut self,
        controls: &[ControlEntry],
    ) -> Result<Option<Vec<StoredEvent>>> {
        let entries = controls
            .iter()
            .cloned()
            .map(SessionLogEntry::Control)
            .collect::<Vec<_>>();
        self.append_bound_application_entries(&entries)
    }

    pub(super) fn append_bound_application_entries(
        &mut self,
        entries: &[SessionLogEntry],
    ) -> Result<Option<Vec<StoredEvent>>> {
        let Some(binding) = self
            .runtime_attachments
            .application_operation
            .clone()
            .filter(|binding| {
                binding.domain_session_scope_id() == self.session_scope_id() && entries.iter().any(|entry|matches!(entry,SessionLogEntry::Control(control) if binding.target.matches(control)))
            })
        else {
            return Ok(None);
        };
        let mut pending = entries
            .iter()
            .map(|entry| PendingStoredEvent {
                event_type: session_entry_event_type(entry),
                event_class: store::session_entry_event_class(session_entry_event_type(entry)),
                payload: serde_json::json!({"session_log_entry":entry}),
                event_id: None,
                correlation_id: None,
                causation_id: None,
            })
            .collect::<Vec<_>>();
        let marker = add_operation_marker(&binding, &mut pending)?;
        let events = pending
            .into_iter()
            .map(|event| {
                Ok((
                    event.event_id.context("operation event id is missing")?,
                    event.event_type,
                    event.event_class,
                    event.payload,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let store = self
            .durable_store()
            .context("application operation lost its durable owner")?;
        let expected_queue = matches!(
            binding.target,
            crate::ApplicationOperationTargetV1::QueueEnqueue { .. }
                | crate::ApplicationOperationTargetV1::QueueEdit { .. }
                | crate::ApplicationOperationTargetV1::QueueCancel { .. }
                | crate::ApplicationOperationTargetV1::QueueReorder { .. }
                | crate::ApplicationOperationTargetV1::QueuePause { .. }
        )
        .then(|| crate::ConversationQueueProjection::from_entries(&self.entries));
        let accepted_run = entries.iter().find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ConversationRunAcceptedV1(accepted)) => {
                Some(accepted.run_id.clone())
            }
            _ => None,
        });
        let expected = binding.clone();
        let appended = store.append_crash_safe_events_if(events, move |records| {
            if let Some(run_id) = accepted_run.as_deref() {
                let prepared_sequence = records.iter().find(|record| record.event_id() == prepared_id(&expected))
                    .context("conversation admission has no prepared command")?.stream_sequence();
                crate::conversation_run::validate_active_conversation_run(records, run_id, prepared_sequence)?;
            }
            validate_record_scope(records, expected.domain_session_scope_id())?;
            if let Some(expected_queue)=expected_queue.as_ref() {
                let entries=records.iter().map(SessionStreamRecord::session_log_entry).collect::<Result<Vec<_>>>()?.into_iter().flatten().collect::<Vec<_>>();
                if &crate::ConversationQueueProjection::from_entries(&entries) != expected_queue {
                    bail!("application queue projection changed before its causal commit");
                }
            }
            if !validate_prepared(records,&expected)? {
                bail!("application operation has no original preparation");
            }
            let committed_id=stable_event_uuid("sigil-application-operation-committed-v1",&expected.operation_id);
            if records.iter().any(|record| record.event_id() == committed_id) {
                bail!("application operation already committed; reconcile instead of executing twice");
            }
            Ok(true)
        })?.context("application operation domain append was not committed")?;
        self.entries.extend_from_slice(entries);
        self.entries.push(SessionLogEntry::Control(marker));
        self.runtime_attachments.application_operation = None;
        Ok(Some(appended))
    }
}

/// Owner-validated durable commit; callers cannot synthesize one from a current projection.
#[derive(Debug, Clone)]
pub struct ApplicationOperationCommitProofV1 {
    marker: StoredEvent,
    matched_control: ControlEntry,
    reasserts_prior_target: bool,
}
impl ApplicationOperationCommitProofV1 {
    /// The exact control authenticated by the committed batch, never a current projection.
    pub fn matched_control(&self) -> &ControlEntry {
        &self.matched_control
    }
    /// Whether an earlier event already satisfied this target before this batch.
    pub fn reasserts_prior_target(&self) -> bool {
        self.reasserts_prior_target
    }
    pub fn validate_binding(&self, binding: &ApplicationOperationBindingV1) -> Result<()> {
        let value = self
            .marker
            .payload
            .get("session_log_entry")
            .context("operation proof lost its marker")?;
        let SessionLogEntry::Control(ControlEntry::ApplicationOperationCommittedV1(commit)) =
            serde_json::from_value(value.clone())?
        else {
            bail!("operation proof is not a commit marker");
        };
        if &commit.binding != binding
            || self.source_session_scope_id() != binding.domain_session_scope_id()
        {
            bail!("operation proof changes its original K/F or domain binding");
        }
        Ok(())
    }
    pub fn source_session_scope_id(&self) -> &str {
        self.marker.session_id.as_str()
    }
    pub fn event_id(&self) -> &str {
        &self.marker.event_id
    }
    pub fn stream_sequence(&self) -> u64 {
        self.marker.stream_sequence
    }
    pub fn record_checksum(&self) -> &str {
        &self.marker.record_checksum
    }
}

/// Bounded streaming reconciliation. Only this operation's at most 1024 batch records are kept.
pub fn reconcile_application_operation(
    reader: &SessionRecordReadHandle,
    binding: &ApplicationOperationBindingV1,
) -> Result<Option<ApplicationOperationCommitProofV1>> {
    binding.validate()?;
    let mut reducer = ApplicationOperationReconciler::new(binding);
    let mut offset = 0;
    let mut sequence = 0;
    loop {
        let page = reader.read_event_record_range(
            offset,
            sequence,
            Some(binding.domain_session_scope_id()),
            128,
            4 * 1024 * 1024,
        )?;
        for record in page.records() {
            if let Some(proof) = reducer.apply(record)? {
                return Ok(Some(proof));
            }
        }
        if !page.has_more() {
            return Ok(None);
        }
        offset = page.end_offset();
        sequence = page
            .records()
            .last()
            .context("application operation scan made no progress")?
            .stream_sequence();
    }
}

/// Reconciles records already read and validated under an actual resource owner's lease.
/// This shares every preparation, identity, contiguous-batch and payload rule with the reader.
pub fn reconcile_application_operation_records(
    records: &[SessionStreamRecord],
    binding: &ApplicationOperationBindingV1,
) -> Result<Option<ApplicationOperationCommitProofV1>> {
    binding.validate()?;
    let mut reducer = ApplicationOperationReconciler::new(binding);
    for record in records {
        if let Some(proof) = reducer.apply(record)? {
            return Ok(Some(proof));
        }
    }
    Ok(None)
}

struct ApplicationOperationReconciler<'a> {
    binding: &'a ApplicationOperationBindingV1,
    domain: VecDeque<(SessionStreamRecord, usize)>,
    retained_bytes: usize,
    prepared: bool,
    first_matching_event: Option<String>,
}
impl<'a> ApplicationOperationReconciler<'a> {
    fn new(binding: &'a ApplicationOperationBindingV1) -> Self {
        Self {
            binding,
            domain: VecDeque::new(),
            retained_bytes: 0,
            prepared: false,
            first_matching_event: None,
        }
    }
    fn apply(
        &mut self,
        record: &SessionStreamRecord,
    ) -> Result<Option<ApplicationOperationCommitProofV1>> {
        let binding = self.binding;
        if self.first_matching_event.is_none()
            && let Some(SessionLogEntry::Control(control)) = record.session_log_entry()?
            && binding.target.matches(&control)
        {
            self.first_matching_event = Some(record.event_id().to_owned());
        }
        if record.session_id() != binding.domain_session_scope_id() {
            bail!("application operation proof has a different source scope");
        }
        if let Some(SessionLogEntry::Control(ControlEntry::ApplicationOperationPreparedV1(prior))) =
            record.session_log_entry()?
            && prior.reservation_key_digest == binding.reservation_key_digest
        {
            if &prior != binding {
                bail!("application operation query does not match original K/F");
            }
            self.prepared = true;
        }
        let Some(SessionLogEntry::Control(ControlEntry::ApplicationOperationCommittedV1(commit))) =
            record.session_log_entry()?
        else {
            let bytes = crate::event::canonical_json_bytes(&record.stored_event().payload)?.len();
            self.retained_bytes += bytes;
            self.domain.push_back((record.clone(), bytes));
            while self.domain.len() > 1024 || self.retained_bytes > MAX_OPERATION_BATCH_BYTES {
                if let Some((_, bytes)) = self.domain.pop_front() {
                    self.retained_bytes -= bytes;
                }
            }
            return Ok(None);
        };
        if commit.binding.operation_id != binding.operation_id {
            return Ok(None);
        }
        commit.validate()?;
        if !self.prepared || &commit.binding != binding {
            bail!("application operation commit lacks its original preparation");
        }
        let mut expected_sequence = record
            .stream_sequence()
            .checked_sub(commit.domain_events.len() as u64)
            .context("application operation sequence underflow")?;
        let mut matched_control = None;
        for evidence in &commit.domain_events {
            let actual = self
                .domain
                .iter()
                .find(|(record, _)| record.event_id() == evidence.event_id)
                .map(|(record, _)| record)
                .context("application operation lost its domain event")?;
            if actual.stream_sequence() != expected_sequence
                || crate::sha256_hex(&crate::event::canonical_json_bytes(
                    &actual.stored_event().payload,
                )?) != evidence.payload_digest
            {
                bail!("application operation domain batch identity changed");
            }
            if let Some(SessionLogEntry::Control(control)) = actual.session_log_entry()?
                && binding.target.matches(&control)
                && matched_control.is_none()
            {
                matched_control = Some((control, actual.event_id().to_owned()));
            }
            expected_sequence += 1;
        }
        let (matched_control, matched_event_id) = matched_control
            .context("application operation domain batch does not satisfy its original target")?;
        Ok(Some(ApplicationOperationCommitProofV1 {
            marker: record.stored_event().clone(),
            matched_control,
            reasserts_prior_target: self
                .first_matching_event
                .as_ref()
                .is_some_and(|first| first != &matched_event_id),
        }))
    }
}

/// Finds only the original operation named by a durable key digest and validates its actual
/// commit before exposing the continuation binding. The lookup does not change the old key.
pub fn committed_application_operation(
    reader: &SessionRecordReadHandle,
    session_scope_id: &str,
    key_digest: &str,
) -> Result<Option<ApplicationOperationBindingV1>> {
    let mut offset = 0;
    let mut sequence = 0;
    let mut found = None;
    loop {
        let page = reader.read_event_record_range(
            offset,
            sequence,
            Some(session_scope_id),
            128,
            4 * 1024 * 1024,
        )?;
        for record in page.records() {
            if let Some(SessionLogEntry::Control(ControlEntry::ApplicationOperationPreparedV1(
                binding,
            ))) = record.session_log_entry()?
                && binding.reservation_key_digest == key_digest
            {
                binding.validate()?;
                if found.as_ref().is_some_and(|previous| previous != &binding) {
                    bail!("original application operation binding conflicts");
                }
                found = Some(binding);
            }
        }
        if !page.has_more() {
            break;
        }
        offset = page.end_offset();
        sequence = page
            .records()
            .last()
            .context("operation lookup made no progress")?
            .stream_sequence();
    }
    match found {
        Some(binding) if reconcile_application_operation(reader, &binding)?.is_some() => {
            Ok(Some(binding))
        }
        _ => Ok(None),
    }
}
