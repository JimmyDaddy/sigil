use super::writer::PendingStoredEvent;
use std::collections::BTreeSet;

use super::*;
use crate::{
    ConversationRunFinalizedEntryV1, ConversationRunLifecycleRecordV1, PublicRunEvent,
    PublicRunEventKind, PublicTaskEventProjector,
};

/// An explicit, source-bound public projection for one provider-visible session entry.
///
/// The caller provides the source position inside its original durable bundle. The session
/// validates the typed source-to-DTO mapping before the writer sees either record, so a private
/// entry can never become public through a generic serde fallback.
#[derive(Debug, Clone)]
pub struct SessionPublicEventProjectionV1 {
    source_entry_index: usize,
    event: crate::RunEvent,
}

impl SessionPublicEventProjectionV1 {
    #[must_use]
    pub fn assistant_message(source_entry_index: usize, message: ModelMessage) -> Self {
        Self {
            source_entry_index,
            event: crate::RunEvent::AssistantMessage(message),
        }
    }

    #[must_use]
    pub fn tool_result(source_entry_index: usize, result: ToolResult) -> Self {
        Self {
            source_entry_index,
            event: crate::RunEvent::ToolResult(result),
        }
    }

    #[must_use]
    pub fn usage_snapshot(source_entry_index: usize, usage: UsageStats) -> Self {
        Self {
            source_entry_index,
            event: crate::RunEvent::Usage(usage),
        }
    }

    /// Validates that the caller's public DTO is the explicit safe projection of its source.
    fn validate_source(&self, entry: &SessionLogEntry) -> Result<()> {
        validate_explicit_session_publication(entry, &self.public_event())
    }

    pub(crate) fn source_entry_index(&self) -> usize {
        self.source_entry_index
    }

    pub(crate) fn public_event(&self) -> PublicRunEventKind {
        self.event.clone().into()
    }

    pub(crate) fn into_run_event(self) -> crate::RunEvent {
        self.event
    }
}

pub(super) fn validate_explicit_session_publication(
    entry: &SessionLogEntry,
    event: &PublicRunEventKind,
) -> Result<()> {
    ExplicitSessionPublicationFingerprint::from_entry(entry)?
        .context("public session projection does not match its durable source type")?
        .validate(event)
}

/// Equality proof for the explicitly public source fields, without retaining source bodies.
#[derive(Debug)]
pub(super) enum ExplicitSessionPublicationFingerprint {
    Assistant(String),
    ToolResult(String),
    Usage(String),
}

impl ExplicitSessionPublicationFingerprint {
    pub(super) fn from_entry(entry: &SessionLogEntry) -> Result<Option<Self>> {
        Ok(match entry {
            SessionLogEntry::Assistant(source) => {
                Some(Self::Assistant(publication_fingerprint(&(
                    &source.id,
                    &source.content,
                    &source.tool_calls,
                    &source.assistant_kind,
                ))?))
            }
            SessionLogEntry::ToolResultV3(recorded) => {
                Some(Self::ToolResult(publication_fingerprint(&(
                    &recorded.call_id,
                    &recorded.tool_name,
                    &recorded.display_view().preview,
                ))?))
            }
            SessionLogEntry::Control(ControlEntry::UsageSnapshot(source)) => {
                Some(Self::Usage(publication_fingerprint(source)?))
            }
            _ => None,
        })
    }

    pub(super) fn validate(&self, event: &PublicRunEventKind) -> Result<()> {
        let (expected, actual) = match (self, event) {
            (Self::Assistant(expected), PublicRunEventKind::AssistantMessage { message }) => (
                expected,
                publication_fingerprint(&(
                    &message.id,
                    &message.content,
                    &message.tool_calls,
                    &message.assistant_kind,
                ))?,
            ),
            (Self::ToolResult(expected), PublicRunEventKind::ToolResult { result }) => (
                expected,
                publication_fingerprint(&(&result.call_id, &result.tool_name, &result.content))?,
            ),
            (Self::Usage(expected), PublicRunEventKind::Usage { usage }) => {
                (expected, publication_fingerprint(usage)?)
            }
            _ => bail!("public session projection does not match its durable source type"),
        };
        if expected != &actual {
            if matches!(self, Self::ToolResult(_)) {
                bail!("public tool result does not match its bounded durable display view");
            }
            bail!("public session projection does not match its durable source type");
        }
        Ok(())
    }
}

fn publication_fingerprint(material: &impl serde::Serialize) -> Result<String> {
    crate::event::canonical_json_content_hash(&serde_json::to_value(material)?)
}

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
    finish_explicit_control_projection(control, events)
}

pub(super) fn finish_explicit_control_projection(
    control: &ControlEntry,
    events: Vec<PublicRunEventKind>,
) -> Result<Vec<PublicRunEventKind>> {
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
    /// Validates a provider-visible bundle before either the private-child fallback or the
    /// public outbox path writes its first byte.
    pub(crate) fn validate_session_publication_bundle(
        &self,
        entries: &[SessionLogEntry],
        publications: &[SessionPublicEventProjectionV1],
    ) -> Result<()> {
        if entries.is_empty() {
            bail!("public session publication requires at least one source entry");
        }
        validate_session_entries_for_publication(entries, self)?;
        let mut published_sources = BTreeSet::new();
        for publication in publications {
            if !published_sources.insert(publication.source_entry_index()) {
                bail!("public session publication requires exactly one projection per source");
            }
            let source = entries
                .get(publication.source_entry_index())
                .context("public session publication source index is outside its bundle")?;
            publication.validate_source(source)?;
        }
        Ok(())
    }

    /// Commits an existing provider-visible session-entry bundle and its explicit public DTOs in
    /// one recoverable writer intent. Entries with no public projection remain domain-only and do
    /// not consume a public sequence.
    ///
    /// # Errors
    ///
    /// Returns an error when a source/projection pair is invalid, the session is not durable, the
    /// public run is stale, or writer recovery cannot prove the original complete bundle.
    pub fn append_session_entries_with_public_outbox(
        &mut self,
        entries: Vec<SessionLogEntry>,
        publications: Vec<SessionPublicEventProjectionV1>,
        run_id: &str,
        next_sequence: u64,
    ) -> Result<(Vec<StoredEvent>, Vec<PublicEventOutboxEntryV1>)> {
        if entries.is_empty()
            || publications.is_empty()
            || run_id.trim().is_empty()
            || next_sequence == 0
        {
            bail!(
                "public session publication requires entries, projections, and an active run sequence"
            );
        }
        let store = self
            .store
            .as_ref()
            .context("public session publication requires a durable session")?;
        self.validate_session_publication_bundle(&entries, &publications)?;

        let mut publications_by_source =
            BTreeMap::<usize, Vec<SessionPublicEventProjectionV1>>::new();
        for publication in publications {
            publications_by_source
                .entry(publication.source_entry_index())
                .or_default()
                .push(publication);
        }

        let mut candidate = self.control_public_projection.clone();
        candidate.refresh(&self.entries)?;
        let mut sequence = next_sequence;
        let mut pending = Vec::with_capacity(entries.len() + publications_by_source.len());
        let mut outbox = Vec::new();
        for (entry_index, entry) in entries.iter().enumerate() {
            let domain_id = uuid::Uuid::new_v4().to_string();
            let event_type = super::store::session_entry_event_type(entry);
            pending.push(PendingStoredEvent {
                event_type,
                event_class: super::store::session_entry_event_class(event_type),
                payload: serde_json::json!({ "session_log_entry": entry }),
                event_id: Some(domain_id.clone()),
                correlation_id: Some(domain_id.clone()),
                causation_id: None,
            });
            for publication in publications_by_source
                .remove(&entry_index)
                .unwrap_or_default()
            {
                let public = PublicRunEvent::new(
                    self.session_scope_id.clone(),
                    run_id,
                    sequence,
                    publication.public_event(),
                );
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
                    bail!("session publication cannot create a run terminal");
                }
                pending.push(PendingStoredEvent {
                    event_type: DurableEventType::PublicEventOutbox,
                    event_class: EventClass::Critical,
                    payload: serde_json::to_value(&entry)?,
                    event_id: Some(public_event_id),
                    correlation_id: Some(domain_id.clone()),
                    causation_id: Some(domain_id.clone()),
                });
                outbox.push(entry);
                sequence = sequence
                    .checked_add(1)
                    .context("public session publication sequence exhausted")?;
            }
        }
        let events = store.append_control_publication(pending, run_id)?;
        let domain_events = events
            .into_iter()
            .filter(|event| event.event_kind() != Some(DurableEventType::PublicEventOutbox))
            .collect::<Vec<_>>();
        self.bind_tool_artifacts_after_append(&entries, &domain_events);
        self.entries.extend(entries);
        candidate.refresh(&self.entries)?;
        self.control_public_projection = candidate;
        self.advance_durable_session_entry_count(&domain_events);
        Ok((domain_events, outbox))
    }

    /// Commits a mixed plan-review terminal bundle in one recoverable writer intent. The source
    /// entries (including private controls and an assistant final answer), their explicit public
    /// projections, the conversation terminal lifecycle record, and its terminal outbox entry
    /// become one contiguous durable batch. This prevents a crash between plan-review closure,
    /// final-answer persistence, and the enclosing conversation terminal from leaving a split
    /// visible state.
    pub fn append_session_entries_with_terminal_outbox(
        &mut self,
        entries: Vec<SessionLogEntry>,
        publications: Vec<SessionPublicEventProjectionV1>,
        terminal: ConversationRunFinalizedEntryV1,
        terminal_event: PublicRunEventKind,
        run_id: &str,
        next_sequence: u64,
    ) -> Result<(Vec<StoredEvent>, Vec<PublicEventOutboxEntryV1>)> {
        if entries.is_empty()
            || run_id.trim().is_empty()
            || next_sequence == 0
            || terminal.run_id() != run_id
        {
            bail!(
                "terminal session publication requires entries, an active run, and matching terminal identity"
            );
        }
        let store = self
            .store
            .as_ref()
            .context("terminal session publication requires a durable session")?;
        self.validate_session_publication_bundle(&entries, &publications)?;
        terminal.validate_shape()?;
        if !super::public_event_outbox::is_terminal_event(&terminal_event) {
            bail!("terminal session publication requires a terminal public event");
        }

        let mut publications_by_source =
            BTreeMap::<usize, Vec<SessionPublicEventProjectionV1>>::new();
        for publication in publications {
            publications_by_source
                .entry(publication.source_entry_index())
                .or_default()
                .push(publication);
        }

        let mut candidate = self.control_public_projection.clone();
        candidate.refresh(&self.entries)?;
        let mut sequence = next_sequence;
        let mut pending = Vec::with_capacity(entries.len() + publications_by_source.len() + 2);
        let mut outbox = Vec::new();
        for (entry_index, entry) in entries.iter().enumerate() {
            let domain_id = uuid::Uuid::new_v4().to_string();
            let event_type = super::store::session_entry_event_type(entry);
            pending.push(PendingStoredEvent {
                event_type,
                event_class: super::store::session_entry_event_class(event_type),
                payload: serde_json::json!({ "session_log_entry": entry }),
                event_id: Some(domain_id.clone()),
                correlation_id: Some(domain_id.clone()),
                causation_id: None,
            });

            let mut public_events = if let SessionLogEntry::Control(control) = entry {
                project_explicit_control(&mut candidate.projector, control)?
            } else {
                Vec::new()
            };
            let explicit_publications = publications_by_source
                .remove(&entry_index)
                .unwrap_or_default();
            if !public_events.is_empty() && !explicit_publications.is_empty() {
                bail!("terminal session source cannot mix control and explicit public projections");
            }
            public_events.extend(
                explicit_publications
                    .into_iter()
                    .map(|publication| publication.public_event()),
            );
            for event in public_events {
                let public =
                    PublicRunEvent::new(self.session_scope_id.clone(), run_id, sequence, event);
                let public_event_id = format!(
                    "application-public:{}:{run_id}:{sequence}",
                    self.session_scope_id
                );
                let outbox_entry = PublicEventOutboxEntryV1 {
                    schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
                    domain_event_id: domain_id.clone(),
                    public_event_id: public_event_id.clone(),
                    run_id: run_id.to_owned(),
                    sequence,
                    payload_digest: stable_event_hash(&serde_json::to_vec(&public)?),
                    event: public,
                };
                if super::public_event_outbox::is_terminal_event(&outbox_entry.event.event) {
                    bail!("terminal session source publication cannot create a run terminal");
                }
                pending.push(PendingStoredEvent {
                    event_type: DurableEventType::PublicEventOutbox,
                    event_class: EventClass::Critical,
                    payload: serde_json::to_value(&outbox_entry)?,
                    event_id: Some(public_event_id),
                    correlation_id: Some(domain_id.clone()),
                    causation_id: Some(domain_id.clone()),
                });
                outbox.push(outbox_entry);
                sequence = sequence
                    .checked_add(1)
                    .context("terminal session publication sequence exhausted")?;
            }
        }

        let terminal_domain_id = uuid::Uuid::new_v4().to_string();
        let terminal_public = PublicRunEvent::new(
            self.session_scope_id.clone(),
            run_id,
            sequence,
            terminal_event,
        );
        let terminal_public_event_id = format!(
            "application-public:{}:{run_id}:{sequence}",
            self.session_scope_id
        );
        let terminal_outbox = PublicEventOutboxEntryV1 {
            schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            domain_event_id: terminal_domain_id.clone(),
            public_event_id: terminal_public_event_id.clone(),
            run_id: run_id.to_owned(),
            sequence,
            payload_digest: stable_event_hash(&serde_json::to_vec(&terminal_public)?),
            event: terminal_public,
        };
        crate::conversation_run::validate_terminal_outbox(&terminal, &terminal_outbox)?;
        pending.push(PendingStoredEvent {
            event_type: DurableEventType::RunFinalized,
            event_class: EventClass::Critical,
            payload: serde_json::to_value(
                ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(terminal),
            )?,
            event_id: Some(terminal_domain_id),
            correlation_id: None,
            causation_id: None,
        });
        pending.push(PendingStoredEvent {
            event_type: DurableEventType::PublicEventOutbox,
            event_class: EventClass::Critical,
            payload: serde_json::to_value(&terminal_outbox)?,
            event_id: Some(terminal_public_event_id),
            correlation_id: None,
            causation_id: None,
        });
        outbox.push(terminal_outbox);

        let events = store.append_control_publication(pending, run_id)?;
        let domain_events = events
            .into_iter()
            .filter(|event| event.event_kind() != Some(DurableEventType::PublicEventOutbox))
            .collect::<Vec<_>>();
        self.bind_tool_artifacts_after_append(&entries, &domain_events);
        self.entries.extend(entries);
        candidate.entry_count = self.entries.len();
        self.control_public_projection = candidate;
        self.advance_durable_session_entry_count(&domain_events);
        Ok((domain_events, outbox))
    }

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
        let operation_marker = if let Some(binding) = self
            .runtime_attachments
            .application_operation
            .as_ref()
            .filter(|binding| {
                binding.domain_session_scope_id() == self.session_scope_id()
                    && controls
                        .iter()
                        .any(|control| binding.target.matches(control))
            }) {
            Some(super::application_operation::add_operation_marker(
                binding,
                &mut pending,
            )?)
        } else {
            None
        };
        let events = store.append_control_publication(pending, run_id)?;
        let domain_events = events
            .into_iter()
            .filter(|event| event.event_kind() != Some(DurableEventType::PublicEventOutbox))
            .collect::<Vec<_>>();
        self.entries
            .extend(controls.into_iter().map(SessionLogEntry::Control));
        if let Some(marker) = operation_marker {
            self.entries.push(SessionLogEntry::Control(marker));
            self.runtime_attachments.application_operation = None;
        }
        candidate.entry_count = self.entries.len();
        self.control_public_projection = candidate;
        self.advance_durable_session_entry_count(&domain_events);
        Ok((domain_events, outbox))
    }
}

fn validate_session_entries_for_publication(
    entries: &[SessionLogEntry],
    session: &Session,
) -> Result<()> {
    for entry in entries {
        match entry {
            SessionLogEntry::ToolResultV3(result) => result.validate()?,
            SessionLogEntry::RuntimeContextSnapshotV2(snapshot) => snapshot.validate()?,
            SessionLogEntry::Control(control) => {
                control.validate_durable_contract()?;
                session.validate_user_input_controls(std::iter::once(control))?;
                session.validate_plan_controls(std::iter::once(control))?;
                if matches!(control, ControlEntry::ConversationInputPromoted(_)) {
                    bail!(
                        "conversation input promotion requires its dedicated critical append API"
                    );
                }
            }
            SessionLogEntry::User(_) | SessionLogEntry::Assistant(_) => {}
        }
    }
    Ok(())
}

/// Validates every source-linked public DTO on replay without creating missing events. Old
/// controls without publications remain readable; a linked publication must be complete,
/// adjacent to its source, and equal to its explicit typed projection at that durable prefix.
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
        let Some(source_entry) = record.session_log_entry()? else {
            continue;
        };
        let Some(publications) = linked.remove(source.event_id.as_str()) else {
            // A domain-only record may carry context without promising a public DTO. Preserve
            // that distinction when rebuilding old/private prefixes; only linked sources must
            // satisfy the stricter explicit-publication projection contract.
            if let SessionLogEntry::Control(control) = source_entry {
                projector.project_control(&control)?;
            }
            continue;
        };
        if source.event_kind() != Some(super::store::session_entry_event_type(&source_entry)) {
            bail!("public session source uses an incorrect durable event type");
        }
        let run_id = &publications[0].run_id;
        if !active_run_ids.contains(run_id) {
            bail!("public session publication source is outside its active durable run");
        }
        let explicit = matches!(
            &source_entry,
            SessionLogEntry::Assistant(_)
                | SessionLogEntry::ToolResultV3(_)
                | SessionLogEntry::Control(ControlEntry::UsageSnapshot(_))
        );
        let expected = if explicit {
            if publications.len() != 1 {
                bail!("explicit public session projection must have exactly one DTO");
            }
            if let SessionLogEntry::Control(control) = &source_entry {
                projector.project_control(control)?;
            }
            None
        } else {
            let SessionLogEntry::Control(control) = &source_entry else {
                bail!("public session publication source has no explicit projection contract");
            };
            Some(project_explicit_control(&mut projector, control)?)
        };
        if let Some(expected) = expected.as_ref()
            && publications.len() != expected.len()
        {
            bail!("public control publication is missing part of its source projection");
        }
        for (ordinal, entry) in publications.iter().enumerate() {
            let envelope = records
                .get(index + ordinal + 1)
                .context("public session publication is detached from its source")?
                .stored_event();
            if envelope.event_id != entry.public_event_id
                || envelope.causation_id.as_deref() != Some(source.event_id.as_str())
                || envelope.correlation_id != source.correlation_id
                || entry.event.session_id != source.session_id
                || entry.run_id != *run_id
            {
                bail!("public session publication does not match its durable envelope");
            }
            if let Some(expected) = expected.as_ref() {
                if serde_json::to_value(&entry.event.event)?
                    != serde_json::to_value(&expected[ordinal])?
                {
                    bail!("public control publication does not match its exact source projection");
                }
            } else {
                validate_explicit_session_publication(&source_entry, &entry.event.event)?;
            }
        }
    }
    if !linked.is_empty() {
        bail!("public session publication has no durable source entry");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/control_publication_tests.rs"]
mod tests;
