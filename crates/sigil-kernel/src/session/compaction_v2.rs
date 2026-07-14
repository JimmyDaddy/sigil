use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::compaction_sidecar::{ContinuationCheckpointV1, validate_pending_applied_sidecar};
use super::*;
use crate::{EventId, SessionId, projection_apply_decision};

/// Schema version used by the V2 compaction lifecycle projection cursor.
pub const COMPACTION_LIFECYCLE_PROJECTION_SCHEMA_VERSION: u16 = 1;

/// Stable domain identity for one successfully activated V2 compaction.
pub type CompactionId = String;

/// Stable domain identity for one V2 compaction attempt.
pub type CompactionAttemptId = String;

/// Durable cursor delimiting the raw session stream folded by a compaction attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionCursor {
    pub session_id: SessionId,
    pub through_stream_sequence: u64,
    pub through_event_id: EventId,
}

impl CompactionCursor {
    pub(crate) fn validate_for_session(
        &self,
        session_id: &str,
        terminal_sequence: u64,
    ) -> Result<()> {
        if self.session_id != session_id {
            bail!("compaction cursor session does not match applied event session");
        }
        if self.through_stream_sequence == 0 {
            bail!("compaction cursor stream sequence must be non-zero");
        }
        if self.through_stream_sequence >= terminal_sequence {
            bail!("compaction cursor must precede its applied event");
        }
        if self.through_event_id.trim().is_empty() {
            bail!("compaction cursor event id is empty");
        }
        Ok(())
    }
}

/// The only fallback parent shape available before provider-observed continuation candidates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum CompactionFallbackParent {
    Root,
    InitiatedAttempt { attempt_id: CompactionAttemptId },
}

/// What explicitly admitted one initiated V2 compaction attempt.
///
/// The automatic scope is a stable, content-free fingerprint of the safe fold material and the
/// effective target policy.  It lets the reducer recover a failed idle-auto attempt after a
/// restart without treating an unrelated later turn as the same retry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum CompactionInitiation {
    Manual,
    IdleAutomatic {
        scope_fingerprint: String,
    },
    /// An exact queued conversation input exceeded its locally proven target budget.
    ///
    /// The associated promotion and its queue-revision CAS are recorded independently. This
    /// lifecycle source is intentionally content-free and does not imply that the queued input
    /// was delivered.
    PreTurnPressure {
        queue_id: crate::ConversationInputQueueId,
    },
    /// A provider proved that one initial conversation request exceeded context before
    /// generation, and the worker is preparing one separate portable lifecycle.
    ///
    /// The source attempt id creates an auditable link to that rejection; it does not authorize
    /// a restart-time replay of the failed provider request.
    OverflowRecovery {
        source_physical_attempt_id: crate::ProviderPhysicalAttemptId,
    },
}

impl CompactionInitiation {
    fn validate_shape(&self) -> Result<()> {
        match self {
            Self::Manual => {}
            Self::IdleAutomatic { scope_fingerprint } if scope_fingerprint.trim().is_empty() => {
                bail!("idle automatic compaction scope fingerprint is empty");
            }
            Self::IdleAutomatic { .. } => {}
            Self::PreTurnPressure { queue_id } => {
                let _validated = crate::ConversationInputQueueId::new(queue_id.as_str())?;
            }
            Self::OverflowRecovery {
                source_physical_attempt_id,
            } if source_physical_attempt_id.trim().is_empty()
                || source_physical_attempt_id.chars().any(char::is_control) =>
            {
                bail!("overflow recovery source physical attempt id is invalid");
            }
            Self::OverflowRecovery { .. } => {}
        }
        Ok(())
    }
}

/// Reason an initiated V2 compaction attempt reached its failure terminal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionFailureReason {
    RecoveryInterrupted,
    ValidationFailed,
    ExecutionFailed,
}

/// Recovery-critical record that opens one initiated V2 compaction attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionStartedEntry {
    pub attempt_id: CompactionAttemptId,
    pub fallback_parent: CompactionFallbackParent,
    pub initiation: CompactionInitiation,
    pub base_projection_revision: String,
    pub started_at_unix_ms: u64,
}

impl CompactionStartedEntry {
    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.attempt_id.trim().is_empty() {
            bail!("compaction attempt id is empty");
        }
        if self.base_projection_revision.trim().is_empty() {
            bail!("compaction base projection revision is empty");
        }
        if let CompactionFallbackParent::InitiatedAttempt { attempt_id } = &self.fallback_parent
            && attempt_id.trim().is_empty()
        {
            bail!("compaction fallback parent attempt id is empty");
        }
        self.initiation.validate_shape()?;
        Ok(())
    }
}

/// Recovery-critical terminal that activates one V2 compaction boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionAppliedV2 {
    pub compaction_id: CompactionId,
    pub attempt_id: CompactionAttemptId,
    pub parent_compaction_id: Option<CompactionId>,
    pub branch_id: Option<crate::BranchId>,
    pub valid_for_snapshot: Option<crate::WorkspaceSnapshotId>,
    pub task_memory_id: Option<crate::TaskMemoryId>,
    pub checkpoint: ContinuationCheckpointV1,
    pub base_projection_revision: String,
    pub folded_through: CompactionCursor,
    pub applied_at_unix_ms: u64,
}

impl CompactionAppliedV2 {
    pub(crate) fn validate_shape(&self, session_id: &str, terminal_sequence: u64) -> Result<()> {
        if self.compaction_id.trim().is_empty() {
            bail!("compaction id is empty");
        }
        if self.attempt_id.trim().is_empty() {
            bail!("compaction applied attempt id is empty");
        }
        if self
            .parent_compaction_id
            .as_deref()
            .is_some_and(|id| id.trim().is_empty())
        {
            bail!("compaction parent id is empty");
        }
        if self
            .branch_id
            .as_deref()
            .is_some_and(|branch| branch.trim().is_empty())
        {
            bail!("compaction branch id is empty");
        }
        if self
            .valid_for_snapshot
            .as_deref()
            .is_some_and(|snapshot| snapshot.trim().is_empty())
        {
            bail!("compaction snapshot id is empty");
        }
        if self
            .task_memory_id
            .as_deref()
            .is_some_and(|memory_id| memory_id.trim().is_empty())
        {
            bail!("compaction task memory id is empty");
        }
        self.checkpoint.validate_shape()?;
        if self.base_projection_revision.trim().is_empty() {
            bail!("compaction applied base projection revision is empty");
        }
        self.folded_through
            .validate_for_session(session_id, terminal_sequence)
    }
}

/// Recovery-critical terminal for an initiated V2 compaction attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionFailureEntry {
    pub attempt_id: CompactionAttemptId,
    pub reason: CompactionFailureReason,
    pub failed_at_unix_ms: u64,
}

impl CompactionFailureEntry {
    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.attempt_id.trim().is_empty() {
            bail!("compaction failure attempt id is empty");
        }
        Ok(())
    }
}

/// The terminal state of a compaction attempt reconstructed from the V2 stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionAttemptTerminal {
    Applied {
        event_id: EventId,
        stream_sequence: u64,
        entry: Box<CompactionAppliedV2>,
    },
    Failed {
        event_id: EventId,
        stream_sequence: u64,
        entry: CompactionFailureEntry,
    },
}

/// One initiated compaction attempt reconstructed from the durable stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionAttemptState {
    pub started_event_id: EventId,
    pub started_stream_sequence: u64,
    pub entry: CompactionStartedEntry,
    pub terminal: Option<CompactionAttemptTerminal>,
}

/// Read-only reconstruction of V2 initiated compaction lifecycle state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactionLifecycleProjection {
    cursor: Option<ProjectionCursor>,
    attempts: BTreeMap<CompactionAttemptId, CompactionAttemptState>,
    applied_compactions: BTreeMap<CompactionId, CompactionAttemptId>,
}

impl CompactionLifecycleProjection {
    /// Rebuilds lifecycle state from a validated V2 session stream without mutating it.
    ///
    /// # Errors
    ///
    /// Returns an error when a lifecycle event is malformed, conflicts with an earlier event, or
    /// violates cursor ordering, terminal uniqueness, or fallback lineage.
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let mut projection = Self::default();
        for record in records {
            projection.apply_record(record)?;
        }
        Ok(projection)
    }

    /// Applies one V2 session record to this read-only projection.
    ///
    /// # Errors
    ///
    /// Returns an error for a sequence conflict, an invalid durable event, or an invalid
    /// compaction lifecycle transition.
    pub fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        let event = record.stored_event();
        let decision = projection_apply_decision(self.cursor.as_ref(), event)?;
        if decision == ProjectionApplyDecision::IgnoreAlreadyApplied {
            return Ok(());
        }

        let decoded = decode_stored_event(event.clone())?;
        match decoded {
            StoredEventDecode::Known(_) | StoredEventDecode::UnknownNonCritical(_) => {}
        }

        match event.event_kind() {
            Some(DurableEventType::CompactionStarted) => {
                let entry: CompactionStartedEntry = decode_compaction_payload(event)?;
                self.apply_started(event, entry)?;
            }
            Some(DurableEventType::CompactionAppliedV2) => {
                let entry: CompactionAppliedV2 = decode_compaction_payload(event)?;
                self.apply_applied(event, entry)?;
            }
            Some(DurableEventType::CompactionFailed) => {
                let entry: CompactionFailureEntry = decode_compaction_payload(event)?;
                self.apply_failed(event, entry)?;
            }
            Some(_) | None => {}
        }

        self.cursor =
            Some(record.projection_cursor(COMPACTION_LIFECYCLE_PROJECTION_SCHEMA_VERSION));
        Ok(())
    }

    /// Returns the cursor proving the latest event consumed by the projection.
    #[must_use]
    pub fn cursor(&self) -> Option<&ProjectionCursor> {
        self.cursor.as_ref()
    }

    /// Returns one attempt by its stable domain identity.
    #[must_use]
    pub fn attempt(&self, attempt_id: &str) -> Option<&CompactionAttemptState> {
        self.attempts.get(attempt_id)
    }

    /// Returns every initiated attempt that lacks a durable terminal.
    #[must_use]
    pub fn unfinished_attempts(&self) -> Vec<&CompactionAttemptState> {
        self.attempts
            .values()
            .filter(|attempt| attempt.terminal.is_none())
            .collect()
    }

    /// Returns whether a terminal failure has durably latched one exact idle-auto scope.
    ///
    /// A new fold material or target policy produces a different scope fingerprint.  This makes
    /// the latch narrow enough for safe automatic retry without allowing the same failing
    /// material to create another lifecycle attempt after reload.
    #[must_use]
    pub fn has_failed_idle_automatic_scope(&self, scope_fingerprint: &str) -> bool {
        self.attempts.values().any(|attempt| {
            matches!(
                &attempt.entry.initiation,
                CompactionInitiation::IdleAutomatic {
                    scope_fingerprint: attempt_scope,
                } if attempt_scope == scope_fingerprint
            ) && matches!(
                attempt.terminal,
                Some(CompactionAttemptTerminal::Failed { .. })
            )
        })
    }

    pub(crate) fn attempts(&self) -> impl Iterator<Item = &CompactionAttemptState> {
        self.attempts.values()
    }

    pub(crate) fn attempt_for_started_event_id(
        &self,
        event_id: &str,
    ) -> Option<&CompactionAttemptState> {
        self.attempts
            .values()
            .find(|attempt| attempt.started_event_id == event_id)
    }

    pub(super) fn validate_started(&self, entry: &CompactionStartedEntry) -> Result<()> {
        entry.validate_shape()?;
        if self.attempts.contains_key(&entry.attempt_id) {
            bail!(
                "compaction attempt {} was started more than once",
                entry.attempt_id
            );
        }
        if let CompactionFallbackParent::InitiatedAttempt { attempt_id } = &entry.fallback_parent {
            if attempt_id == &entry.attempt_id {
                bail!("compaction attempt cannot fall back to itself");
            }
            let parent = self
                .attempts
                .get(attempt_id)
                .with_context(|| format!("compaction fallback parent {attempt_id} is missing"))?;
            if !matches!(
                parent.terminal,
                Some(CompactionAttemptTerminal::Failed { .. })
            ) {
                bail!("compaction fallback parent {attempt_id} is not durably failed");
            }
        }
        Ok(())
    }

    fn apply_started(&mut self, event: &StoredEvent, entry: CompactionStartedEntry) -> Result<()> {
        if event.correlation_id.as_deref() != Some(event.event_id.as_str()) {
            bail!("compaction start correlation id must equal its event id");
        }
        if event.causation_id.is_some() {
            bail!("compaction start must not have a causation id");
        }
        self.validate_started(&entry)?;
        self.attempts.insert(
            entry.attempt_id.clone(),
            CompactionAttemptState {
                started_event_id: event.event_id.clone(),
                started_stream_sequence: event.stream_sequence,
                entry,
                terminal: None,
            },
        );
        Ok(())
    }

    fn validate_terminal_attempt(
        &self,
        attempt_id: &str,
        base_projection_revision: Option<&str>,
    ) -> Result<&CompactionAttemptState> {
        let attempt = self.attempts.get(attempt_id).with_context(|| {
            format!("compaction terminal references unknown attempt {attempt_id}")
        })?;
        if attempt.terminal.is_some() {
            bail!("compaction attempt {attempt_id} already has a terminal event");
        }
        if let Some(revision) = base_projection_revision
            && attempt.entry.base_projection_revision != revision
        {
            bail!("compaction terminal base projection revision does not match started attempt");
        }
        Ok(attempt)
    }

    fn apply_applied(&mut self, event: &StoredEvent, entry: CompactionAppliedV2) -> Result<()> {
        entry.validate_shape(&event.session_id, event.stream_sequence)?;
        let attempt = self.validate_terminal_attempt(
            &entry.attempt_id,
            Some(entry.base_projection_revision.as_str()),
        )?;
        validate_terminal_lineage(event, attempt)?;
        if self.applied_compactions.contains_key(&entry.compaction_id) {
            bail!(
                "compaction id {} was applied more than once",
                entry.compaction_id
            );
        }
        if let Some(parent_compaction_id) = &entry.parent_compaction_id
            && !self.applied_compactions.contains_key(parent_compaction_id)
        {
            bail!("compaction parent {parent_compaction_id} is missing or not applied");
        }

        let attempt = self
            .attempts
            .get_mut(&entry.attempt_id)
            .expect("validated compaction attempt remains present");
        attempt.terminal = Some(CompactionAttemptTerminal::Applied {
            event_id: event.event_id.clone(),
            stream_sequence: event.stream_sequence,
            entry: Box::new(entry.clone()),
        });
        self.applied_compactions
            .insert(entry.compaction_id.clone(), entry.attempt_id);
        Ok(())
    }

    fn apply_failed(&mut self, event: &StoredEvent, entry: CompactionFailureEntry) -> Result<()> {
        entry.validate_shape()?;
        let attempt = self.validate_terminal_attempt(&entry.attempt_id, None)?;
        validate_terminal_lineage(event, attempt)?;
        let attempt = self
            .attempts
            .get_mut(&entry.attempt_id)
            .expect("validated compaction attempt remains present");
        attempt.terminal = Some(CompactionAttemptTerminal::Failed {
            event_id: event.event_id.clone(),
            stream_sequence: event.stream_sequence,
            entry,
        });
        Ok(())
    }
}

fn validate_terminal_lineage(event: &StoredEvent, attempt: &CompactionAttemptState) -> Result<()> {
    let started_event_id = attempt.started_event_id.as_str();
    if event.correlation_id.as_deref() != Some(started_event_id) {
        bail!("compaction terminal correlation id must reference its started event");
    }
    if event.causation_id.as_deref() != Some(started_event_id) {
        bail!("compaction terminal causation id must reference its started event");
    }
    Ok(())
}

impl JsonlSessionStore {
    /// Appends one initiated V2 compaction attempt as a synced correlation root.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry conflicts with existing lifecycle state or the durable
    /// append fails.
    pub fn append_compaction_started(&self, entry: CompactionStartedEntry) -> Result<StoredEvent> {
        entry.validate_shape()?;
        let session_id = compaction_session_id(self)?;
        let event_id = compaction_lifecycle_event_id(&session_id, &entry.attempt_id, "started");
        let payload = serde_json::to_value(&entry).context("failed to encode compaction start")?;
        let event = self.append_event_if_with_identity(
            DurableEventType::CompactionStarted,
            payload,
            event_id.clone(),
            Some(event_id),
            None,
            |records| {
                let projection = CompactionLifecycleProjection::from_records(records)?;
                projection.validate_started(&entry)?;
                Ok(true)
            },
        )?;
        event.context("compaction start append was not attempted")
    }

    /// Appends the sole applied terminal for an initiated V2 compaction attempt.
    ///
    /// # Errors
    ///
    /// Returns an error when the attempt is missing, already terminal, stale, or the durable
    /// append fails.
    pub fn append_compaction_applied_v2(&self, entry: CompactionAppliedV2) -> Result<StoredEvent> {
        let session_id = compaction_session_id(self)?;
        let started_event_id = compaction_started_event_id(self, &entry.attempt_id)?;
        let event_id = compaction_lifecycle_event_id(&session_id, &entry.attempt_id, "applied");
        let payload =
            serde_json::to_value(&entry).context("failed to encode compaction applied")?;
        let event = self.append_event_if_with_identity(
            DurableEventType::CompactionAppliedV2,
            payload,
            event_id,
            Some(started_event_id.clone()),
            Some(started_event_id),
            |records| {
                let projection = CompactionLifecycleProjection::from_records(records)?;
                projection.validate_terminal_attempt(
                    &entry.attempt_id,
                    Some(entry.base_projection_revision.as_str()),
                )?;
                entry.validate_shape(&session_id, next_stream_sequence(records))?;
                if projection
                    .applied_compactions
                    .contains_key(&entry.compaction_id)
                {
                    bail!(
                        "compaction id {} was applied more than once",
                        entry.compaction_id
                    );
                }
                if let Some(parent_compaction_id) = &entry.parent_compaction_id
                    && !projection
                        .applied_compactions
                        .contains_key(parent_compaction_id)
                {
                    bail!("compaction parent {parent_compaction_id} is missing or not applied");
                }
                validate_pending_applied_sidecar(records, &entry)?;
                Ok(true)
            },
        )?;
        event.context("compaction applied append was not attempted")
    }

    /// Appends the sole failure terminal for an initiated V2 compaction attempt.
    ///
    /// The append is idempotent for recovery: a pre-existing terminal leaves the stream unchanged
    /// and returns `Ok(None)`.
    pub fn append_compaction_failed(
        &self,
        entry: CompactionFailureEntry,
    ) -> Result<Option<StoredEvent>> {
        entry.validate_shape()?;
        let session_id = compaction_session_id(self)?;
        let started_event_id = compaction_started_event_id(self, &entry.attempt_id)?;
        let event_id = compaction_lifecycle_event_id(&session_id, &entry.attempt_id, "failed");
        let payload =
            serde_json::to_value(&entry).context("failed to encode compaction failure")?;
        self.append_event_if_with_identity(
            DurableEventType::CompactionFailed,
            payload,
            event_id,
            Some(started_event_id.clone()),
            Some(started_event_id),
            |records| {
                let projection = CompactionLifecycleProjection::from_records(records)?;
                let attempt = projection.attempt(&entry.attempt_id).with_context(|| {
                    format!(
                        "compaction failure references unknown attempt {}",
                        entry.attempt_id
                    )
                })?;
                Ok(attempt.terminal.is_none())
            },
        )
    }

    /// Explicitly terminates every unfinished initiated attempt as interrupted recovery.
    ///
    /// Ordinary reads never call this method. Repeating it after the first successful recovery
    /// leaves the stream unchanged.
    pub fn recover_unfinished_compaction_attempts(&self, now_unix_ms: u64) -> Result<usize> {
        let records = self.read_event_records_writer()?;
        let projection = CompactionLifecycleProjection::from_records(&records)?;
        let attempt_ids = projection
            .unfinished_attempts()
            .into_iter()
            .map(|attempt| attempt.entry.attempt_id.clone())
            .collect::<Vec<_>>();
        let mut appended = 0;
        for attempt_id in attempt_ids {
            let appended_event = self.append_compaction_failed(CompactionFailureEntry {
                attempt_id,
                reason: CompactionFailureReason::RecoveryInterrupted,
                failed_at_unix_ms: now_unix_ms,
            })?;
            appended += usize::from(appended_event.is_some());
        }
        Ok(appended)
    }
}

pub(super) fn compaction_session_id(store: &JsonlSessionStore) -> Result<SessionId> {
    let records = store.read_event_records_writer()?;
    Ok(stream_session_id(&records).unwrap_or_else(|| session_id_for_path(store.path())))
}

pub(super) fn compaction_lifecycle_event_id(
    session_id: &str,
    attempt_id: &str,
    terminal: &str,
) -> EventId {
    stable_event_uuid(
        "sigil-compaction-v2-lifecycle",
        &format!("{session_id}:{attempt_id}:{terminal}"),
    )
}

pub(super) fn compaction_started_event_id(
    store: &JsonlSessionStore,
    attempt_id: &str,
) -> Result<EventId> {
    let records = store.read_event_records_writer()?;
    let projection = CompactionLifecycleProjection::from_records(&records)?;
    projection
        .attempt(attempt_id)
        .map(|attempt| attempt.started_event_id.clone())
        .with_context(|| format!("compaction attempt {attempt_id} is missing"))
}

fn decode_compaction_payload<T>(event: &StoredEvent) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_value(event.payload.clone())
        .with_context(|| format!("failed to decode {} compaction payload", event.event_type))
}

#[cfg(test)]
#[path = "tests/compaction_v2_tests.rs"]
mod tests;
