//! Attachment-local incremental reducers and physical positions. No durable authority is issued here.
use super::*;
use sigil_kernel::{PublicEventOutboxValidatorV1, SessionReadBudget};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone)]
pub(super) struct RecordPosition {
    pub offset: u64,
    pub end: u64,
    pub sequence: u64,
    pub checksum: String,
    pub tool_name: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ProjectionReadMetrics {
    pub full_prefix_scan_count: u64,
    pub records_applied: u64,
    pub bytes_read: u64,
    pub page_bytes_read: u64,
}

pub(super) struct ProjectionRecordCache {
    pub permanent_error: Option<ApplicationError>,
    pub source: Option<sigil_kernel::SessionRecordSourceSnapshot>,
    pub modified: Option<SystemTime>,
    pub end_offset: u64,
    pub sequence: u64,
    pub session_id: Option<String>,
    pub transcript: Vec<RecordPosition>,
    pub message_content: BTreeMap<String, super::message_content::MessageContentPosition>,
    pub display: crate::conversation_display::ConversationDisplayIndex,
    pub latest_message: Option<String>,
    pub public: Vec<(String, RecordPosition)>,
    pub public_positions: BTreeMap<String, usize>,
    pub delivery: PublicEventOutboxValidatorV1,
    pub queue: ConversationQueueDurableProjection,
    pub terminal: TerminalTaskProjection,
    pub event_state: ProjectionEventState,
    pub route_entries: Vec<SessionLogEntry>,
    pub route_revision: u64,
    waiting_domains: BTreeSet<String>,
    tool_names: BTreeMap<String, String>,
    tool_name_order: VecDeque<String>,
    pub metrics: ProjectionReadMetrics,
}

impl std::fmt::Debug for ProjectionRecordCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectionRecordCache")
            .field("sequence", &self.sequence)
            .field("messages", &self.transcript.len())
            .field("metrics", &self.metrics)
            .finish()
    }
}

impl Default for ProjectionRecordCache {
    fn default() -> Self {
        Self {
            permanent_error: None,
            source: None,
            modified: None,
            end_offset: 0,
            sequence: 0,
            session_id: None,
            transcript: Vec::new(),
            message_content: BTreeMap::new(),
            display: Default::default(),
            latest_message: None,
            public: Vec::new(),
            public_positions: BTreeMap::new(),
            delivery: PublicEventOutboxValidatorV1::default(),
            queue: Default::default(),
            terminal: Default::default(),
            event_state: ProjectionEventState::new(),
            route_entries: Vec::new(),
            route_revision: 0,
            waiting_domains: BTreeSet::new(),
            tool_names: BTreeMap::new(),
            tool_name_order: VecDeque::new(),
            metrics: Default::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(not(unix))]
    created: Option<SystemTime>,
}
impl SourceIdentity {
    pub(super) fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Self {
                dev: metadata.dev(),
                ino: metadata.ino(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                created: metadata.created().ok(),
            }
        }
    }
}

impl ProjectionRecordCache {
    pub fn synchronize(
        &mut self,
        owner: &RuntimeSessionProjectionOwner,
        budget: &SessionReadBudget,
    ) -> Result<(), ApplicationError> {
        budget.check().map_err(unavailable)?;
        let source = owner.reader.source_snapshot(budget).map_err(read_error)?;
        if self
            .source
            .as_ref()
            .is_some_and(|previous| !previous.same_source(&source))
            || source.byte_len() < self.end_offset
            || (self.source.is_some()
                && source.byte_len() == self.end_offset
                && source.modified_at() != self.modified)
        {
            return Err(ApplicationError::ResetRequired);
        }
        if self.source.is_none() {
            self.metrics.full_prefix_scan_count += 1;
        }
        let target = source.byte_len();
        // The source identity survives cooperative interruption. Only the validated offset
        // advances; the unvalidated suffix is read again when this same observer retries.
        self.source = Some(source.clone());
        self.modified = source.modified_at();
        // A single cache lock owns catch-up; only byte-bounded ranges hold the writer coordinator.
        while self.end_offset < target {
            budget.check().map_err(unavailable)?;
            let range = owner
                .reader
                .read_event_record_range_with_budget(
                    self.end_offset,
                    self.sequence,
                    self.session_id.as_deref(),
                    256,
                    (target - self.end_offset).min(MAX_PROJECTION_RANGE_BYTES as u64) as usize,
                    budget,
                )
                .map_err(read_error)?;
            if !range.source_snapshot().same_source(&source) {
                return Err(ApplicationError::ResetRequired);
            }
            if range.end_offset() <= self.end_offset || range.end_offset() > target {
                return Err(ApplicationError::ResetRequired);
            }
            self.metrics.bytes_read += range.end_offset() - self.end_offset;
            for (index, record) in range.records().iter().enumerate() {
                budget.check().map_err(unavailable)?;
                let position = RecordPosition {
                    offset: range.record_offsets()[index],
                    end: range.record_end_offsets()[index],
                    sequence: record.stream_sequence(),
                    checksum: record.stored_event().record_checksum.clone(),
                    tool_name: None,
                };
                let record_end = position.end;
                self.apply_record(record, position)?;
                self.end_offset = record_end;
            }
            self.end_offset = range.end_offset();
        }
        self.delivery.validate_cut().map_err(corrupt)?;
        if self.session_id.is_none() {
            return Err(ApplicationError::Unavailable);
        }
        Ok(())
    }

    fn apply_record(
        &mut self,
        record: &SessionStreamRecord,
        position: RecordPosition,
    ) -> Result<(), ApplicationError> {
        if self.sequence.checked_add(1) != Some(record.stream_sequence()) {
            return Err(ApplicationError::ResetRequired);
        }
        if self
            .session_id
            .as_ref()
            .is_some_and(|id| id != record.session_id())
        {
            return Err(ApplicationError::ScopeMismatch);
        }
        self.session_id
            .get_or_insert_with(|| record.session_id().to_owned());
        self.queue.apply_record(record).map_err(corrupt)?;
        self.delivery.apply_record(record).map_err(corrupt)?;
        self.display
            .apply_record(
                record,
                crate::conversation_display::ConversationDisplayRecordPosition {
                    offset: position.offset,
                    end: position.end,
                    sequence: position.sequence,
                    checksum: position.checksum.clone(),
                },
            )
            .map_err(corrupt)?;
        if let Some(SessionLogEntry::Control(control)) =
            record.session_log_entry().map_err(corrupt)?
        {
            self.terminal.apply_control_entry(&control);
            match &control {
                ControlEntry::SessionIdentity { .. } => {
                    self.route_entries = vec![SessionLogEntry::Control(control.clone())];
                    self.route_revision = record.stream_sequence();
                }
                ControlEntry::SessionModelSelected { .. }
                | ControlEntry::SessionRouteRebound { .. } => {
                    self.route_entries.truncate(1);
                    self.route_entries
                        .push(SessionLogEntry::Control(control.clone()));
                    self.route_revision = record.stream_sequence();
                }
                ControlEntry::SessionRouteTrustBound { .. } => {
                    self.route_entries.retain(|entry| {
                        !matches!(
                            entry,
                            SessionLogEntry::Control(ControlEntry::SessionRouteTrustBound { .. })
                        )
                    });
                    self.route_entries
                        .push(SessionLogEntry::Control(control.clone()));
                    self.route_revision = record.stream_sequence();
                }
                ControlEntry::PlanReviewAttempt(attempt)
                    if attempt.revision_request_id.is_some()
                        && attempt.status
                            == sigil_kernel::PlanReviewAttemptStatus::WaitingForInput =>
                {
                    self.waiting_domains.insert(record.event_id().to_owned());
                }
                _ => {}
            }
        }
        if record.stored_event().event_kind() == Some(DurableEventType::PublicEventOutbox) {
            let entry: PublicEventOutboxEntryV1 =
                serde_json::from_value(record.stored_event().payload.clone()).map_err(corrupt)?;
            let waiting = self.waiting_domains.remove(&entry.domain_event_id);
            self.event_state.apply_event(&entry, waiting);
            self.public_positions
                .insert(entry.public_event_id.clone(), self.public.len());
            self.public.push((entry.public_event_id, position.clone()));
        }
        if let Some(entry) =
            sigil_kernel::conversation_transcript_entry_from_record(record).map_err(corrupt)?
        {
            super::message_content::index_message_content(self, record, &position, &entry)?;
            let ordinal = self.transcript.len() as u64 + 1;
            if let Some(message) = crate::application_run::project_application_transcript_entry(
                entry,
                ordinal,
                &mut self.tool_names,
                &mut self.tool_name_order,
                512,
            )
            .map_err(corrupt)?
            {
                self.latest_message = message.content.filter(|content| !content.is_empty());
                let mut position = position;
                position.tool_name = message.tool_name;
                self.transcript.push(position);
            }
        }
        self.sequence = record.stream_sequence();
        self.metrics.records_applied += 1;
        Ok(())
    }

    pub fn read_position(
        &mut self,
        owner: &RuntimeSessionProjectionOwner,
        position: &RecordPosition,
        budget: &SessionReadBudget,
        page: bool,
    ) -> Result<SessionStreamRecord, ApplicationError> {
        let range = owner
            .reader
            .read_event_record_range_with_budget(
                position.offset,
                position.sequence - 1,
                self.session_id.as_deref(),
                1,
                (position.end - position.offset) as usize,
                budget,
            )
            .map_err(read_error)?;
        if self
            .source
            .as_ref()
            .is_none_or(|source| !source.same_source(range.source_snapshot()))
            || range.source_snapshot().byte_len() < self.end_offset
        {
            return Err(ApplicationError::ResetRequired);
        }
        let record = range
            .records()
            .first()
            .ok_or(ApplicationError::ResetRequired)?;
        if range.end_offset() != position.end
            || record.stored_event().record_checksum != position.checksum
        {
            return Err(ApplicationError::ResetRequired);
        }
        self.metrics.bytes_read += range.end_offset() - range.start_offset();
        if page {
            self.metrics.page_bytes_read += range.end_offset() - range.start_offset();
        }
        Ok(record.clone())
    }

    pub fn transcript_page(
        &mut self,
        owner: &RuntimeSessionProjectionOwner,
        through_sequence: u64,
        before: Option<u64>,
        limit: usize,
        budget: &SessionReadBudget,
    ) -> Result<crate::application_run::ApplicationTranscriptPage, ApplicationError> {
        let total = self
            .transcript
            .partition_point(|position| position.sequence <= through_sequence);
        let end = before
            .map(|before| before.saturating_sub(1).min(total as u64) as usize)
            .unwrap_or(total);
        let start = end.saturating_sub(limit);
        let mut messages = Vec::new();
        let mut raw_bytes = 0u64;
        let mut encoded_bytes = 0usize;
        let mut text_bytes = 0usize;
        for index in (start..end).rev() {
            budget.check().map_err(unavailable)?;
            let position = self.transcript[index].clone();
            if raw_bytes + position.end - position.offset > MAX_PROJECTION_RANGE_BYTES as u64 {
                break;
            }
            let record = self.read_position(owner, &position, budget, true)?;
            raw_bytes += position.end - position.offset;
            let entry = sigil_kernel::conversation_transcript_entry_from_record(&record)
                .map_err(corrupt)?
                .ok_or(ApplicationError::ResetRequired)?;
            let mut message = crate::application_run::project_application_transcript_entry(
                entry,
                index as u64 + 1,
                &mut Default::default(),
                &mut Default::default(),
                512,
            )
            .map_err(corrupt)?
            .ok_or(ApplicationError::ResetRequired)?;
            message.tool_name = position.tool_name;
            let text_size = message.content.as_ref().map_or(0, String::len);
            // Include JSON escaping and a conservative allowance for the bounded metadata.
            let encoded_size = serde_json::to_vec(&message.content).map_err(corrupt)?.len() + 1024;
            if encoded_bytes + encoded_size > 1024 * 1024
                || text_bytes + text_size
                    > crate::application_run::MAX_APPLICATION_TRANSCRIPT_PAGE_BYTES
            {
                break;
            }
            encoded_bytes += encoded_size;
            text_bytes += text_size;
            messages.push(message);
        }
        messages.reverse();
        let next_before = messages
            .first()
            .filter(|message| message.ordinal > 1)
            .map(|message| message.ordinal);
        Ok(crate::application_run::ApplicationTranscriptPage {
            session_scope_id: self
                .session_id
                .clone()
                .ok_or(ApplicationError::Unavailable)?,
            total_messages: total as u64,
            messages,
            next_before,
        })
    }

    pub fn display_page(
        &mut self,
        owner: &RuntimeSessionProjectionOwner,
        request: crate::application_projection::ConversationDisplayQuery<'_>,
        budget: &SessionReadBudget,
    ) -> Result<
        crate::conversation_display::ConversationDisplayPageV1,
        crate::conversation_display::ConversationDisplayProjectionError,
    > {
        let plan = self.display.prepare_page(
            request.expected_session_scope_id,
            request.cursor,
            request.limit,
            request.current_workspace_snapshot_id,
        )?;
        let mut raw_bytes = 0_u64;
        for position in plan.positions() {
            raw_bytes = raw_bytes
                .checked_add(
                    position
                        .end
                        .checked_sub(position.offset)
                        .ok_or_else(|| anyhow::anyhow!("display source bounds are invalid"))?,
                )
                .ok_or_else(|| anyhow::anyhow!("display source budget overflow"))?;
            if raw_bytes > MAX_PROJECTION_RANGE_BYTES as u64 {
                return Err(anyhow::anyhow!("display page source budget exceeded").into());
            }
        }
        let mut records = Vec::with_capacity(plan.positions().len());
        for position in plan.positions() {
            budget.check()?;
            records.push(self.read_position(owner, &RecordPosition { offset: position.offset, end: position.end,
                sequence: position.sequence, checksum: position.checksum.clone(), tool_name: None }, budget, true)
                .map_err(crate::conversation_display::ConversationDisplayProjectionError::from_application)?);
        }
        budget.check()?;
        plan.finish(&records, request.artifact_store)
    }
}

pub(super) fn unavailable(_: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::Unavailable
}
pub(super) fn corrupt(error: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::CorruptProjection(error.to_string())
}

fn read_error(error: anyhow::Error) -> ApplicationError {
    if error.is::<sigil_kernel::SessionObservationCancelled>()
        || error.is::<sigil_kernel::session::SessionIoBusyError>()
    {
        return ApplicationError::Unavailable;
    }
    if let Some(error) = error.downcast_ref::<std::io::Error>() {
        return match error.kind() {
            std::io::ErrorKind::NotFound => ApplicationError::ResetRequired,
            std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof => corrupt(error),
            _ => ApplicationError::Unavailable,
        };
    }
    corrupt(error)
}
