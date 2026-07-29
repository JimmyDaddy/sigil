use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Weak},
};

use anyhow::{Context, Result, bail};

use super::writer::SharedSessionCoordinator;
use super::*;
use crate::{
    AgentThreadId, ConversationQueueDurableProjection, EventId, ReadinessEvaluatedEntry,
    SessionStats, TaskGuidancePromotedEntry, TaskId, TaskPlanStatus, TaskRunStatus, TerminalTaskId,
};

/// Schema version for the bounded scheduler-facing session projection.
pub const ACTIVE_SESSION_PROJECTION_SCHEMA_VERSION: u16 = 2;
const MAX_ACTIVE_TASK_GUIDANCE_STATES: usize = 1_024;
const MAX_RECENT_TERMINAL_TASK_IDS: usize = 256;
const MAX_OPEN_COMPACTION_ATTEMPTS: usize = 16;
const MAX_RECENT_IDLE_COMPACTION_ATTEMPTS: usize = 64;
const MAX_PENDING_AGENT_CONTINUATIONS: usize = 1_024;
const MAX_ACTIVE_TERMINAL_TASKS: usize = 1_024;

/// Privacy-safe process-local counters for the shared active-session coordinator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActiveProjectionMetricsSnapshot {
    pub snapshot_total: u64,
    pub full_rebuild_total: u64,
    pub incremental_apply_total: u64,
    pub invalidation_total: u64,
    pub writer_lock_attempt_total: u64,
    pub publication_total: u64,
}

/// Exact durable frontier represented by an [`ActiveSessionProjectionSnapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveProjectionFrontier {
    pub(crate) writer_generation: String,
    pub(crate) session_id: String,
    pub(crate) durable_end_offset: u64,
    pub(crate) cursor: Option<ProjectionCursor>,
}

impl ActiveProjectionFrontier {
    /// Returns the durable session identity at this frontier.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Returns the last durable record cursor, or `None` for an empty stream.
    #[must_use]
    pub fn cursor(&self) -> Option<&ProjectionCursor> {
        self.cursor.as_ref()
    }

    /// Returns the byte offset immediately after the durable prefix represented here.
    #[must_use]
    pub fn durable_end_offset(&self) -> u64 {
        self.durable_end_offset
    }
}

/// A small task state sufficient to admit task-guidance work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTaskGuidanceState {
    status: TaskRunStatus,
    latest_plan_version: Option<u32>,
    accepted_plan_version: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveCompactionAttempt {
    started_event_id: EventId,
    base_projection_revision: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveIdleCompactionTerminal {
    Applied,
    SemanticFailure,
    OtherFailure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveIdleCompactionAttempt {
    attempt_id: CompactionAttemptId,
    scope_fingerprint: String,
    circuit_scope: Option<CompactionCircuitScopeV1>,
    terminal: Option<ActiveIdleCompactionTerminal>,
}

/// Bounded compaction lifecycle material needed by the scheduler.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActiveCompactionSummary {
    open_attempts: BTreeMap<CompactionAttemptId, ActiveCompactionAttempt>,
    open_attempts_overflowed: bool,
    canonical_validation_required: bool,
    recent_idle_attempts: VecDeque<ActiveIdleCompactionAttempt>,
    latest_applied_compaction_id: Option<CompactionId>,
    latest_applied_stream_sequence: Option<u64>,
    latest_completed_real_turn_sequence: Option<u64>,
    completed_real_turns_since_latest_applied: u8,
}

impl ActiveCompactionSummary {
    /// Returns the number of initiated attempts without a durable terminal.
    #[must_use]
    pub fn open_attempt_count(&self) -> usize {
        self.open_attempts.len()
    }

    pub(super) fn can_start_attempt(&self, attempt_id: &str) -> bool {
        !self.open_attempts.contains_key(attempt_id)
    }

    pub(super) fn started_event_id(&self, attempt_id: &str) -> Option<&str> {
        self.open_attempts
            .get(attempt_id)
            .map(|attempt| attempt.started_event_id.as_str())
    }

    /// Returns the latest activated compaction identity without retaining its checkpoint body.
    #[must_use]
    pub fn latest_applied_compaction_id(&self) -> Option<&str> {
        self.latest_applied_compaction_id.as_deref()
    }

    /// Returns whether one exact idle-automatic scope already has a durable failure terminal.
    ///
    /// The projection retains the latest 64 idle attempts. This is sufficient for the current
    /// source frontier because successful activation or new fold material changes the scope.
    #[must_use]
    pub fn has_failed_idle_automatic_scope(&self, scope_fingerprint: &str) -> bool {
        self.recent_idle_attempts.iter().any(|attempt| {
            attempt.scope_fingerprint == scope_fingerprint
                && matches!(
                    attempt.terminal,
                    Some(
                        ActiveIdleCompactionTerminal::SemanticFailure
                            | ActiveIdleCompactionTerminal::OtherFailure
                    )
                )
        })
    }

    /// Returns the latest successfully activated compaction terminal sequence.
    #[must_use]
    pub fn latest_applied_stream_sequence(&self) -> Option<u64> {
        self.latest_applied_stream_sequence
    }

    /// Returns the latest completed real turn after the latest activated compaction.
    #[must_use]
    pub fn latest_completed_real_turn_sequence(&self) -> Option<u64> {
        self.latest_completed_real_turn_sequence
    }

    /// Returns a saturated count of completed real turns after the latest activation.
    ///
    /// Only zero, one, or more-than-one affects admission, so the incremental reducer caps this
    /// value at two.
    #[must_use]
    pub fn completed_real_turns_since_latest_applied(&self) -> u8 {
        self.completed_real_turns_since_latest_applied
    }

    /// Evaluates the automatic-compaction circuit from the bounded active projection.
    ///
    /// # Errors
    ///
    /// Returns an error when the supplied current scope is malformed.
    pub fn circuit_breaker_decision(
        &self,
        input: &CompactionCircuitBreakerInputV1,
    ) -> Result<CompactionCircuitBreakerDecisionV1> {
        if [
            input.scope.source_cursor_event_id.as_str(),
            input.scope.layout_hash.as_str(),
            input.scope.route_fingerprint.as_str(),
        ]
        .iter()
        .any(|value| value.trim().is_empty() || value.chars().any(char::is_control))
        {
            bail!("compaction circuit scope is invalid");
        }
        if self.recent_idle_attempts.iter().any(|attempt| {
            attempt.circuit_scope.as_ref().is_some_and(|scope| {
                scope.source_cursor_event_id == input.scope.source_cursor_event_id
                    && scope.layout_hash == input.scope.layout_hash
            }) && matches!(
                attempt.terminal,
                Some(
                    ActiveIdleCompactionTerminal::SemanticFailure
                        | ActiveIdleCompactionTerminal::OtherFailure
                )
            )
        }) {
            return Ok(CompactionCircuitBreakerDecisionV1::SameCursorAndLayoutFailed);
        }
        let consecutive_failures =
            self.recent_idle_attempts
                .iter()
                .rev()
                .filter(|attempt| {
                    attempt.circuit_scope.as_ref().is_some_and(|scope| {
                        scope.route_fingerprint == input.scope.route_fingerprint
                    })
                })
                .take_while(|attempt| {
                    attempt.terminal == Some(ActiveIdleCompactionTerminal::SemanticFailure)
                })
                .count()
                .min(usize::from(u8::MAX)) as u8;
        if consecutive_failures >= 2 && !input.manual_retry {
            return Ok(
                CompactionCircuitBreakerDecisionV1::SemanticSummarizerRouteDisabled {
                    consecutive_failures,
                },
            );
        }
        if let (Some(_), Some(layer)) = (
            self.latest_applied_stream_sequence,
            input.post_activation_emergency_layer,
        ) {
            return Ok(CompactionCircuitBreakerDecisionV1::PostActivationEmergency { layer });
        }
        if let Some(latest_compaction_sequence) = self.latest_applied_stream_sequence
            && !input.emergency
            && !input.manual_retry
            && input
                .latest_completed_real_turn_sequence
                .is_none_or(|sequence| sequence <= latest_compaction_sequence)
        {
            return Ok(CompactionCircuitBreakerDecisionV1::RealTurnRequired {
                latest_compaction_sequence,
            });
        }
        Ok(CompactionCircuitBreakerDecisionV1::Allowed)
    }

    fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        let event = record.stored_event();
        match event.event_kind() {
            Some(DurableEventType::CompactionStarted) => {
                let entry: CompactionStartedEntry =
                    serde_json::from_value(event.payload.clone())
                        .context("failed to decode active compaction start")?;
                entry.validate_shape()?;
                if event.correlation_id.as_deref() != Some(event.event_id.as_str())
                    || event.causation_id.is_some()
                {
                    bail!("active compaction start has invalid durable lineage");
                }
                if self.open_attempts.contains_key(&entry.attempt_id) {
                    bail!("active compaction attempt was started more than once");
                }
                if let CompactionInitiation::IdleAutomatic {
                    scope_fingerprint,
                    circuit_scope,
                } = &entry.initiation
                {
                    self.recent_idle_attempts
                        .push_back(ActiveIdleCompactionAttempt {
                            attempt_id: entry.attempt_id.clone(),
                            scope_fingerprint: scope_fingerprint.clone(),
                            circuit_scope: circuit_scope.clone(),
                            terminal: None,
                        });
                    while self.recent_idle_attempts.len() > MAX_RECENT_IDLE_COMPACTION_ATTEMPTS {
                        self.recent_idle_attempts.pop_front();
                    }
                }
                if self.open_attempts_overflowed
                    || self.open_attempts.len() >= MAX_OPEN_COMPACTION_ATTEMPTS
                {
                    self.open_attempts_overflowed = true;
                    self.canonical_validation_required = true;
                    return Ok(());
                }
                self.open_attempts.insert(
                    entry.attempt_id,
                    ActiveCompactionAttempt {
                        started_event_id: event.event_id.clone(),
                        base_projection_revision: entry.base_projection_revision,
                    },
                );
            }
            Some(DurableEventType::CompactionAppliedV2) => {
                let entry: CompactionAppliedV2 = serde_json::from_value(event.payload.clone())
                    .context("failed to decode active compaction terminal")?;
                entry.validate_shape(&event.session_id, event.stream_sequence)?;
                if let Some(attempt) = self.open_attempts.get(&entry.attempt_id) {
                    if attempt.base_projection_revision != entry.base_projection_revision
                        || event.correlation_id.as_deref()
                            != Some(attempt.started_event_id.as_str())
                        || event.causation_id.as_deref() != Some(attempt.started_event_id.as_str())
                    {
                        bail!("active compaction terminal does not match its started attempt");
                    }
                    self.open_attempts.remove(&entry.attempt_id);
                } else if self.open_attempts_overflowed {
                    self.canonical_validation_required = true;
                } else {
                    bail!("active compaction terminal references an unknown attempt");
                }
                self.mark_idle_terminal(&entry.attempt_id, ActiveIdleCompactionTerminal::Applied);
                self.latest_applied_compaction_id = Some(entry.compaction_id);
                self.latest_applied_stream_sequence = Some(event.stream_sequence);
                self.latest_completed_real_turn_sequence = None;
                self.completed_real_turns_since_latest_applied = 0;
            }
            Some(DurableEventType::CompactionFailed) => {
                let entry: CompactionFailureEntry =
                    serde_json::from_value(event.payload.clone())
                        .context("failed to decode active compaction failure")?;
                entry.validate_shape()?;
                if let Some(attempt) = self.open_attempts.get(&entry.attempt_id) {
                    if event.correlation_id.as_deref() != Some(attempt.started_event_id.as_str())
                        || event.causation_id.as_deref() != Some(attempt.started_event_id.as_str())
                    {
                        bail!("active compaction failure does not match its started attempt");
                    }
                    self.open_attempts.remove(&entry.attempt_id);
                } else if self.open_attempts_overflowed {
                    self.canonical_validation_required = true;
                } else {
                    bail!("active compaction failure references an unknown attempt");
                }
                let terminal = if matches!(
                    entry.reason,
                    CompactionFailureReason::SemanticSummaryTimeout
                        | CompactionFailureReason::SemanticSummaryInflated
                        | CompactionFailureReason::SemanticSummaryInvalid
                ) {
                    ActiveIdleCompactionTerminal::SemanticFailure
                } else {
                    ActiveIdleCompactionTerminal::OtherFailure
                };
                self.mark_idle_terminal(&entry.attempt_id, terminal);
            }
            Some(DurableEventType::AssistantMessageRecorded) => {
                if self.latest_applied_stream_sequence.is_some() {
                    self.latest_completed_real_turn_sequence = Some(event.stream_sequence);
                    self.completed_real_turns_since_latest_applied = self
                        .completed_real_turns_since_latest_applied
                        .saturating_add(1)
                        .min(2);
                }
            }
            Some(_) | None => {}
        }
        Ok(())
    }

    fn mark_idle_terminal(&mut self, attempt_id: &str, terminal: ActiveIdleCompactionTerminal) {
        if let Some(attempt) = self
            .recent_idle_attempts
            .iter_mut()
            .rev()
            .find(|attempt| attempt.attempt_id == attempt_id)
        {
            attempt.terminal = Some(terminal);
        }
    }

    fn canonical_validation_required(&self) -> bool {
        self.canonical_validation_required
    }

    fn mark_canonical_validated(&mut self) {
        self.canonical_validation_required = false;
    }
}

impl ActiveTaskGuidanceState {
    /// Returns the latest durable task-run status relevant to guidance admission.
    #[must_use]
    pub fn status(&self) -> TaskRunStatus {
        self.status
    }

    /// Returns the latest non-superseded plan version observed for this task.
    #[must_use]
    pub fn latest_plan_version(&self) -> Option<u32> {
        self.latest_plan_version
    }

    /// Returns the currently accepted plan version, when one remains accepted.
    #[must_use]
    pub fn accepted_plan_version(&self) -> Option<u32> {
        self.accepted_plan_version
    }
}

/// Scheduler-facing read model incrementally reduced from the durable session stream.
///
/// The projection intentionally excludes transcript bodies, terminal output and task-plan steps.
/// Its collections contain only current queue, unresolved continuation and active terminal state.
#[derive(Debug, Clone)]
pub struct ActiveSessionProjection {
    queue: ConversationQueueDurableProjection,
    task_guidance: BTreeMap<TaskId, ActiveTaskGuidanceState>,
    task_guidance_overflowed: bool,
    recent_terminal_tasks: VecDeque<(TaskId, TaskRunStatus)>,
    recent_terminal_tasks_truncated: bool,
    compaction: ActiveCompactionSummary,
    pending_agent_continuations: BTreeSet<AgentThreadId>,
    pending_agent_continuations_overflowed: bool,
    active_terminal_tasks: BTreeSet<TerminalTaskId>,
    active_terminal_tasks_overflowed: bool,
    usage: SessionStats,
    latest_readiness: Option<ReadinessEvaluatedEntry>,
    tool_output_pressure: ToolOutputPressureProjectionV1,
    durable_session_entry_count: u64,
    last_session_entry_cursor: Option<ProjectionCursor>,
    cursor: Option<ProjectionCursor>,
    pub(crate) frontier: ActiveProjectionFrontier,
}

impl ActiveSessionProjection {
    pub(super) fn compaction_summary(&self) -> &ActiveCompactionSummary {
        &self.compaction
    }

    pub(super) fn tool_output_pressure_snapshot(&self) -> ToolOutputPressureSnapshotV1 {
        self.tool_output_pressure.snapshot()
    }

    pub(super) fn from_records(
        records: &[SessionStreamRecord],
        frontier: ActiveProjectionFrontier,
    ) -> Result<Self> {
        CompactionLifecycleProjection::from_records(records)?;
        ConversationQueueDurableProjection::from_records(records)?;
        let mut projection = Self {
            queue: ConversationQueueDurableProjection::default(),
            task_guidance: BTreeMap::new(),
            task_guidance_overflowed: false,
            recent_terminal_tasks: VecDeque::new(),
            recent_terminal_tasks_truncated: false,
            compaction: ActiveCompactionSummary::default(),
            pending_agent_continuations: BTreeSet::new(),
            pending_agent_continuations_overflowed: false,
            active_terminal_tasks: BTreeSet::new(),
            active_terminal_tasks_overflowed: false,
            usage: SessionStats::default(),
            latest_readiness: None,
            tool_output_pressure: ToolOutputPressureProjectionV1::default(),
            durable_session_entry_count: 0,
            last_session_entry_cursor: None,
            cursor: None,
            frontier: frontier.clone(),
        };
        for record in records {
            projection.apply_record(record)?;
        }
        projection.compaction.mark_canonical_validated();
        projection.queue.retain_active_queue_ids();
        if projection.cursor != frontier.cursor {
            bail!("active projection frontier does not match the durable stream");
        }
        projection.frontier = frontier;
        Ok(projection)
    }

    pub(super) fn apply_records(
        &mut self,
        records: &[SessionStreamRecord],
        frontier: ActiveProjectionFrontier,
    ) -> Result<()> {
        if self.frontier.writer_generation != frontier.writer_generation {
            bail!("active projection writer generation changed");
        }
        for record in records {
            self.apply_record(record)?;
        }
        if self.cursor != frontier.cursor {
            bail!("active projection delta does not reach the durable frontier");
        }
        self.frontier = frontier;
        Ok(())
    }

    pub(super) fn compaction_canonical_validation_required(&self) -> bool {
        self.compaction.canonical_validation_required()
    }

    pub(super) fn validate_compaction_canonical_records(
        &mut self,
        records: &[SessionStreamRecord],
    ) -> Result<()> {
        CompactionLifecycleProjection::from_records(records)?;
        self.compaction.mark_canonical_validated();
        Ok(())
    }

    fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        if self.cursor.as_ref().is_some_and(|cursor| {
            cursor.projection_schema_version != ACTIVE_SESSION_PROJECTION_SCHEMA_VERSION
        }) {
            bail!("active projection schema version mismatch");
        }
        if self.cursor.is_none() && record.stream_sequence() != 1 {
            bail!("active projection prefix does not start at sequence one");
        }
        let next_cursor = record.projection_cursor(ACTIVE_SESSION_PROJECTION_SCHEMA_VERSION);
        match projection_apply_decision_for_record(
            self.cursor.as_ref(),
            &next_cursor.session_id,
            next_cursor.last_applied_stream_sequence,
            &next_cursor.last_applied_event_id,
            &next_cursor.last_applied_record_checksum,
        )? {
            ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
            ProjectionApplyDecision::Apply => {}
        }

        // Each reducer validates its own durable payload family before the global cursor advances.
        self.queue.apply_record(record)?;
        self.queue.retain_active_queue_ids();
        self.compaction.apply_record(record)?;
        self.tool_output_pressure
            .apply_records(std::slice::from_ref(record))?;
        if let Some(entry) = record.session_log_entry()? {
            self.durable_session_entry_count = self
                .durable_session_entry_count
                .checked_add(1)
                .context("active projection session entry count overflow")?;
            self.last_session_entry_cursor = Some(next_cursor.clone());
            if let SessionLogEntry::Control(control) = entry {
                self.apply_control_entry(&control)?;
            }
        }
        self.cursor = Some(next_cursor);
        Ok(())
    }

    fn apply_control_entry(&mut self, control: &ControlEntry) -> Result<()> {
        match control {
            ControlEntry::TaskRun(entry) => {
                if matches!(
                    entry.status,
                    TaskRunStatus::Completed | TaskRunStatus::Cancelled
                ) {
                    self.task_guidance.remove(&entry.task_id);
                    self.recent_terminal_tasks
                        .retain(|(task_id, _)| task_id != &entry.task_id);
                    self.recent_terminal_tasks
                        .push_back((entry.task_id.clone(), entry.status));
                    while self.recent_terminal_tasks.len() > MAX_RECENT_TERMINAL_TASK_IDS {
                        self.recent_terminal_tasks.pop_front();
                        self.recent_terminal_tasks_truncated = true;
                    }
                    return Ok(());
                }
                if self
                    .recent_terminal_tasks
                    .iter()
                    .any(|(task_id, _)| task_id == &entry.task_id)
                {
                    // Match the canonical task reducer: completed/cancelled tasks remain final, so
                    // a late non-final status is ignored instead of invalidating the whole active
                    // projection. Older terminal ids may be evicted from this bounded hint; the
                    // final TaskGuidance writer CAS therefore revalidates canonical task history.
                    return Ok(());
                }
                if !self.task_guidance.contains_key(&entry.task_id)
                    && (self.task_guidance_overflowed
                        || self.task_guidance.len() >= MAX_ACTIVE_TASK_GUIDANCE_STATES)
                {
                    // This is a bounded acceleration cache, not durable write authority. Once
                    // saturated, keep the projection valid and let a guidance lookup miss trigger
                    // the canonical task replay/CAS path.
                    self.task_guidance_overflowed = true;
                    return Ok(());
                }
                let current = self.task_guidance.entry(entry.task_id.clone()).or_insert(
                    ActiveTaskGuidanceState {
                        status: entry.status,
                        latest_plan_version: None,
                        accepted_plan_version: None,
                    },
                );
                if !matches!(
                    current.status,
                    TaskRunStatus::Completed | TaskRunStatus::Cancelled
                ) || current.status == entry.status
                {
                    current.status = entry.status;
                }
            }
            ControlEntry::TaskPlan(entry) => {
                if self
                    .recent_terminal_tasks
                    .iter()
                    .any(|(task_id, _)| task_id == &entry.task_id)
                {
                    return Ok(());
                }
                let Some(current) = self.task_guidance.get_mut(&entry.task_id) else {
                    return Ok(());
                };
                if entry.status != TaskPlanStatus::Superseded {
                    current.latest_plan_version = Some(entry.plan_version);
                }
                match entry.status {
                    TaskPlanStatus::Accepted => {
                        current.accepted_plan_version = Some(entry.plan_version);
                    }
                    TaskPlanStatus::Proposed
                    | TaskPlanStatus::Rejected
                    | TaskPlanStatus::Superseded
                        if current.accepted_plan_version == Some(entry.plan_version) =>
                    {
                        current.accepted_plan_version = None;
                    }
                    TaskPlanStatus::Proposed
                    | TaskPlanStatus::Rejected
                    | TaskPlanStatus::Superseded => {}
                }
            }
            ControlEntry::AgentResultContinuation(entry) => {
                if entry.status.is_unresolved() {
                    if !self.pending_agent_continuations.contains(&entry.thread_id)
                        && (self.pending_agent_continuations_overflowed
                            || self.pending_agent_continuations.len()
                                >= MAX_PENDING_AGENT_CONTINUATIONS)
                    {
                        self.pending_agent_continuations_overflowed = true;
                        return Ok(());
                    }
                    self.pending_agent_continuations
                        .insert(entry.thread_id.clone());
                } else {
                    self.pending_agent_continuations.remove(&entry.thread_id);
                }
            }
            ControlEntry::TerminalTask(entry) => {
                if entry.status.is_active() {
                    if !self.active_terminal_tasks.contains(&entry.handle.task_id)
                        && (self.active_terminal_tasks_overflowed
                            || self.active_terminal_tasks.len() >= MAX_ACTIVE_TERMINAL_TASKS)
                    {
                        self.active_terminal_tasks_overflowed = true;
                        return Ok(());
                    }
                    self.active_terminal_tasks
                        .insert(entry.handle.task_id.clone());
                } else {
                    self.active_terminal_tasks.remove(&entry.handle.task_id);
                }
            }
            ControlEntry::UsageSnapshot(usage) => self.usage.apply_usage(usage),
            ControlEntry::SemanticCompactionUsageSnapshot(usage) => {
                self.usage.apply_semantic_compaction_usage(usage);
            }
            ControlEntry::ReadinessEvaluated(entry) => {
                self.latest_readiness = Some(entry.clone());
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn queue(&self) -> &ConversationQueueDurableProjection {
        &self.queue
    }

    pub(super) fn task_guidance_requires_canonical_validation(&self) -> bool {
        self.task_guidance_overflowed || self.recent_terminal_tasks_truncated
    }

    pub(super) fn validate_task_guidance_promotion(
        &self,
        entry: &TaskGuidancePromotedEntry,
    ) -> Result<()> {
        self.queue.validate_task_guidance_promotion(entry)?;
        let task = self
            .task_guidance
            .get(&entry.task_id)
            .context("task guidance promotion references an unknown task")?;
        if matches!(
            task.status,
            TaskRunStatus::Completed | TaskRunStatus::Cancelled
        ) {
            bail!("task guidance promotion cannot target a completed or cancelled task");
        }
        if task.latest_plan_version != Some(entry.plan_version)
            || task.accepted_plan_version != Some(entry.plan_version)
        {
            bail!("task guidance promotion plan version is not the accepted task plan");
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn inject_schema_mismatch(&mut self) {
        if let Some(cursor) = self.cursor.as_mut() {
            cursor.projection_schema_version =
                ACTIVE_SESSION_PROJECTION_SCHEMA_VERSION.saturating_add(1);
        }
    }
}

/// Immutable active projection paired with the exact durable frontier it represents.
#[derive(Debug, Clone)]
pub struct ActiveSessionProjectionSnapshot {
    pub(crate) projection: Arc<ActiveSessionProjection>,
}

impl ActiveSessionProjectionSnapshot {
    /// Returns the durable frontier represented by this snapshot.
    #[must_use]
    pub fn frontier(&self) -> &ActiveProjectionFrontier {
        &self.projection.frontier
    }

    /// Returns the current scheduler queue and its exact revision.
    #[must_use]
    pub fn conversation_queue(&self) -> &ConversationQueueDurableProjection {
        &self.projection.queue
    }

    /// Returns the minimal task state used to admit guidance for `task_id`.
    #[must_use]
    pub fn task_guidance_state(&self, task_id: &TaskId) -> Option<&ActiveTaskGuidanceState> {
        self.projection.task_guidance.get(task_id)
    }

    /// Returns whether a task-state cache miss requires canonical replay before being interpreted.
    #[must_use]
    pub fn task_guidance_may_be_incomplete(&self) -> bool {
        self.projection.task_guidance_overflowed
    }

    /// Returns whether `task_id` recently reached a final completed/cancelled state and was
    /// evicted from the active guidance map.
    #[must_use]
    pub fn is_recent_terminal_task(&self, task_id: &TaskId) -> bool {
        self.recent_terminal_task_status(task_id).is_some()
    }

    /// Returns the precise recent final status evicted from the active guidance map.
    #[must_use]
    pub fn recent_terminal_task_status(&self, task_id: &TaskId) -> Option<TaskRunStatus> {
        self.projection
            .recent_terminal_tasks
            .iter()
            .rev()
            .find_map(|(candidate, status)| (candidate == task_id).then_some(*status))
    }

    /// Returns unresolved child-result continuations only.
    #[must_use]
    pub fn pending_agent_continuations(&self) -> &BTreeSet<AgentThreadId> {
        &self.projection.pending_agent_continuations
    }

    /// Returns whether unresolved continuation ids exceeded the bounded active cache.
    #[must_use]
    pub fn pending_agent_continuations_may_be_incomplete(&self) -> bool {
        self.projection.pending_agent_continuations_overflowed
    }

    /// Returns active terminal task identities without command or output bodies.
    #[must_use]
    pub fn active_terminal_tasks(&self) -> &BTreeSet<TerminalTaskId> {
        &self.projection.active_terminal_tasks
    }

    /// Returns whether active terminal task ids exceeded the bounded active cache.
    #[must_use]
    pub fn active_terminal_tasks_may_be_incomplete(&self) -> bool {
        self.projection.active_terminal_tasks_overflowed
    }

    /// Returns bounded compaction scheduler state without checkpoint or summary bodies.
    #[must_use]
    pub fn compaction(&self) -> &ActiveCompactionSummary {
        &self.projection.compaction
    }

    /// Returns accumulated provider usage at this durable frontier.
    #[must_use]
    pub fn usage(&self) -> &SessionStats {
        &self.projection.usage
    }

    /// Returns the latest readiness evaluation across scopes.
    #[must_use]
    pub fn latest_readiness(&self) -> Option<&ReadinessEvaluatedEntry> {
        self.projection.latest_readiness.as_ref()
    }

    /// Returns body-free tool-output pressure at the exact active durable frontier.
    #[must_use]
    pub fn tool_output_pressure(&self) -> ToolOutputPressureSnapshotV1 {
        self.projection.tool_output_pressure.snapshot()
    }

    /// Returns the exact number of durable records projecting to a [`SessionLogEntry`].
    #[must_use]
    pub fn durable_session_entry_count(&self) -> u64 {
        self.projection.durable_session_entry_count
    }

    /// Returns the latest durable session-entry cursor, excluding later durable-only records.
    #[must_use]
    pub fn last_session_entry_cursor(&self) -> Option<&ProjectionCursor> {
        self.projection.last_session_entry_cursor.as_ref()
    }

    /// Returns a conservative heap-aware size estimate for the bounded projection.
    ///
    /// This is evidence telemetry rather than an allocator-exact measurement. It includes owned
    /// collection members and string payloads instead of reporting only the inline struct size.
    #[must_use]
    pub fn approximate_memory_bytes(&self) -> usize {
        let projection = self.projection.as_ref();
        let queue_bytes = projection
            .queue
            .queue
            .items
            .iter()
            .map(|item| {
                std::mem::size_of_val(item)
                    + item.queued.queue_id.as_str().len()
                    + item.queued.prompt.len()
                    + item.queued.prompt_hash.len()
                    + item.reason.as_deref().map_or(0, str::len)
            })
            .sum::<usize>();
        let task_bytes = projection
            .task_guidance
            .keys()
            .map(|task_id| std::mem::size_of::<ActiveTaskGuidanceState>() + task_id.as_str().len())
            .sum::<usize>();
        let recent_terminal_bytes = projection
            .recent_terminal_tasks
            .iter()
            .map(|(task_id, _)| task_id.as_str().len() + std::mem::size_of::<TaskRunStatus>())
            .sum::<usize>();
        let continuation_bytes = projection
            .pending_agent_continuations
            .iter()
            .map(|thread_id| thread_id.as_str().len())
            .sum::<usize>();
        let terminal_bytes = projection
            .active_terminal_tasks
            .iter()
            .map(|task_id| task_id.as_str().len())
            .sum::<usize>();
        std::mem::size_of_val(projection)
            + queue_bytes
            + task_bytes
            + recent_terminal_bytes
            + continuation_bytes
            + terminal_bytes
    }
}

/// Projection publication delivered after the durable append and projection commit.
#[derive(Debug, Clone)]
pub struct ActiveProjectionNotice {
    /// The newly published durable frontier.
    pub frontier: ActiveProjectionFrontier,
    /// Whether the projection is usable without rebuilding.
    pub valid: bool,
    /// Typed projection families changed by the committed durable delta.
    pub changed_families: BTreeSet<ActiveProjectionFamily>,
}

/// One independently dirty-able scheduler projection family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActiveProjectionFamily {
    Queue,
    Task,
    Compaction,
    AgentContinuation,
    TerminalTask,
    Usage,
    Readiness,
    ToolOutputPressure,
}

impl ActiveProjectionFamily {
    pub(super) fn all() -> BTreeSet<Self> {
        [
            Self::Queue,
            Self::Task,
            Self::Compaction,
            Self::AgentContinuation,
            Self::TerminalTask,
            Self::Usage,
            Self::Readiness,
            Self::ToolOutputPressure,
        ]
        .into_iter()
        .collect()
    }
}

/// Observer for active projection publication.
///
/// Callbacks run after writer and projection locks have been released. Implementations must avoid
/// blocking because notification is synchronous with append acknowledgement.
pub trait ActiveProjectionObserver: Send + Sync {
    /// Observes one durable projection publication.
    fn active_projection_changed(&self, notice: ActiveProjectionNotice);
}

/// Drop guard for one active projection observer registration.
pub struct ActiveProjectionSubscription {
    pub(super) coordinator: Weak<SharedSessionCoordinator>,
    pub(super) observer_id: u64,
    pub(super) _observer: Arc<dyn ActiveProjectionObserver>,
}

impl std::fmt::Debug for ActiveProjectionSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActiveProjectionSubscription")
            .field("observer_id", &self.observer_id)
            .finish_non_exhaustive()
    }
}

impl Drop for ActiveProjectionSubscription {
    fn drop(&mut self) {
        if let Some(coordinator) = self.coordinator.upgrade() {
            coordinator.unregister_observer(self.observer_id);
        }
    }
}

#[cfg(test)]
#[path = "tests/active_projection_tests.rs"]
mod tests;
