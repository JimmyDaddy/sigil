use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::compaction_v2::{
    CompactionAttemptTerminal, CompactionLifecycleProjection, compaction_lifecycle_event_id,
    compaction_session_id, compaction_started_event_id,
};
use super::*;
use crate::{EventId, ProjectionCursor, projection_apply_decision};

/// Current schema for an attempt-bound, durable tool-output projection sidecar.
pub const TOOL_OUTPUT_PROJECTION_SIDECAR_SCHEMA_VERSION: u16 = 1;
/// Read-only projection cursor schema for tool-output shrink sidecars.
pub const TOOL_OUTPUT_PROJECTION_SIDECAR_PROJECTION_SCHEMA_VERSION: u16 = 1;
/// Current schema for an explicit tool-output context-epoch transition.
pub const TOOL_OUTPUT_CONTEXT_EPOCH_TRANSITION_SCHEMA_VERSION: u16 = 1;

/// Why a prepared shrink projection is allowed to replace provider-visible historical bytes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputContextEpochTransitionReasonV1 {
    SemanticCompaction,
    StandaloneShrink,
}

/// Explicit proof that shrink activation rotates, rather than invisibly mutates, a cache epoch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolOutputContextEpochTransitionV1 {
    pub schema_version: u16,
    pub source_epoch_id: String,
    pub target_epoch_id: String,
    pub reason: ToolOutputContextEpochTransitionReasonV1,
}

impl ToolOutputContextEpochTransitionV1 {
    /// Creates an explicit standalone-shrink epoch transition.
    ///
    /// # Errors
    ///
    /// Returns an error when the source and target are empty or identical.
    pub fn standalone(
        source_epoch_id: impl Into<String>,
        target_epoch_id: impl Into<String>,
    ) -> Result<Self> {
        let transition = Self {
            schema_version: TOOL_OUTPUT_CONTEXT_EPOCH_TRANSITION_SCHEMA_VERSION,
            source_epoch_id: source_epoch_id.into(),
            target_epoch_id: target_epoch_id.into(),
            reason: ToolOutputContextEpochTransitionReasonV1::StandaloneShrink,
        };
        transition.validate()?;
        Ok(transition)
    }

    fn semantic(plan: &CompactionFoldPlan, compaction_id: &str) -> Self {
        let source_epoch_id = plan.prior_folded_through.as_ref().map_or_else(
            || "context-epoch:root".to_owned(),
            |cursor| format!("context-epoch:{}", cursor.through_event_id),
        );
        Self {
            schema_version: TOOL_OUTPUT_CONTEXT_EPOCH_TRANSITION_SCHEMA_VERSION,
            source_epoch_id,
            target_epoch_id: format!("context-epoch:{compaction_id}"),
            reason: ToolOutputContextEpochTransitionReasonV1::SemanticCompaction,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != TOOL_OUTPUT_CONTEXT_EPOCH_TRANSITION_SCHEMA_VERSION
            || self.source_epoch_id.trim().is_empty()
            || self.target_epoch_id.trim().is_empty()
            || self.source_epoch_id == self.target_epoch_id
        {
            bail!("tool-output context epoch transition is invalid");
        }
        Ok(())
    }
}

/// Durable binding of an exact old-tool-output projection to one applied V2 compaction.
///
/// The sidecar stores no source output text. Each descriptor proves its source event and the
/// deterministic head/tail projection is rebuilt from the immutable raw transcript on load.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolOutputProjectionShrinkRecorded {
    pub schema_version: u16,
    pub compaction_id: CompactionId,
    pub attempt_id: CompactionAttemptId,
    /// Exact V2 stream tail observed before the compaction Started barrier was appended.
    pub source_plan_cursor: ProjectionCursor,
    pub requested_tail_message_count: usize,
    /// Exact V3 whole-turn selection proof, when the source plan used adaptive tail planning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adaptive_tail: Option<AdaptiveTailSelectionV3>,
    /// Previous activated boundary used while planning a repeated compaction, if any.
    pub prior_folded_through: Option<super::compaction_v2::CompactionCursor>,
    pub policy: ToolOutputProjectionPolicy,
    pub shrinks: Vec<ToolOutputProjectionShrink>,
    /// New sidecars always bind shrink activation to a distinct epoch. Legacy sidecars decode
    /// without this field and retain their V2 compatibility behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch_transition: Option<ToolOutputContextEpochTransitionV1>,
}

impl ToolOutputProjectionShrinkRecorded {
    /// Creates a durable sidecar from an exact pre-Start fold plan and its deterministic output.
    ///
    /// # Errors
    ///
    /// Returns an error when the projection is empty or does not describe the supplied plan.
    pub fn from_projection(
        compaction_id: impl Into<CompactionId>,
        attempt_id: impl Into<CompactionAttemptId>,
        plan: &CompactionFoldPlan,
        policy: ToolOutputProjectionPolicy,
        projection: &ToolOutputProjection,
    ) -> Result<Self> {
        let compaction_id = compaction_id.into();
        let entry = Self {
            schema_version: TOOL_OUTPUT_PROJECTION_SIDECAR_SCHEMA_VERSION,
            compaction_id: compaction_id.clone(),
            attempt_id: attempt_id.into(),
            source_plan_cursor: plan.base_stream_cursor.clone(),
            requested_tail_message_count: plan.requested_tail_message_count,
            adaptive_tail: plan.adaptive_tail.clone(),
            prior_folded_through: plan.prior_folded_through.clone(),
            policy,
            shrinks: projection
                .outputs
                .iter()
                .map(|output| output.shrink.clone())
                .collect(),
            epoch_transition: Some(ToolOutputContextEpochTransitionV1::semantic(
                plan,
                &compaction_id,
            )),
        };
        entry.validate_shape()?;
        Ok(entry)
    }

    /// Creates a standalone shrink epoch without publishing a semantic checkpoint or fold
    /// boundary. The supplied plan is used only to prove that every selected tool result belongs
    /// to old, complete history rather than the active raw tail.
    ///
    /// # Errors
    ///
    /// Returns an error when the epoch transition or projection is empty or malformed.
    pub fn standalone(
        source_epoch_id: impl Into<String>,
        target_epoch_id: impl Into<String>,
        plan: &CompactionFoldPlan,
        policy: ToolOutputProjectionPolicy,
        projection: &ToolOutputProjection,
    ) -> Result<Self> {
        let source_epoch_id = source_epoch_id.into();
        let target_epoch_id = target_epoch_id.into();
        let entry = Self {
            schema_version: TOOL_OUTPUT_PROJECTION_SIDECAR_SCHEMA_VERSION,
            compaction_id: target_epoch_id.clone(),
            attempt_id: format!("standalone-shrink-attempt:{target_epoch_id}"),
            source_plan_cursor: plan.base_stream_cursor.clone(),
            requested_tail_message_count: plan.requested_tail_message_count,
            adaptive_tail: plan.adaptive_tail.clone(),
            prior_folded_through: plan.prior_folded_through.clone(),
            policy,
            shrinks: projection
                .outputs
                .iter()
                .map(|output| output.shrink.clone())
                .collect(),
            epoch_transition: Some(ToolOutputContextEpochTransitionV1::standalone(
                source_epoch_id,
                target_epoch_id,
            )?),
        };
        entry.validate_shape()?;
        Ok(entry)
    }

    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.schema_version != TOOL_OUTPUT_PROJECTION_SIDECAR_SCHEMA_VERSION {
            bail!("unsupported tool-output projection sidecar schema version");
        }
        if self.compaction_id.trim().is_empty() || self.attempt_id.trim().is_empty() {
            bail!("tool-output projection sidecar compaction and attempt ids are required");
        }
        if self.source_plan_cursor.projection_schema_version != COMPACTION_FOLD_PLAN_SCHEMA_VERSION
            || self.source_plan_cursor.session_id.trim().is_empty()
            || self.source_plan_cursor.last_applied_stream_sequence == 0
            || self
                .source_plan_cursor
                .last_applied_event_id
                .trim()
                .is_empty()
            || self
                .source_plan_cursor
                .last_applied_record_checksum
                .trim()
                .is_empty()
        {
            bail!("tool-output projection sidecar source plan cursor is invalid");
        }
        if self.requested_tail_message_count == 0 {
            bail!("tool-output projection sidecar must retain a raw tail");
        }
        if let Some(adaptive_tail) = &self.adaptive_tail {
            if adaptive_tail.schema_version != ADAPTIVE_TAIL_SELECTION_SCHEMA_VERSION {
                bail!("tool-output projection sidecar adaptive tail schema is unsupported");
            }
            adaptive_tail.policy.validate()?;
            if adaptive_tail.exact_fit_limit_tokens == 0 {
                bail!("tool-output projection sidecar adaptive exact-fit limit is invalid");
            }
        }
        self.policy.validate()?;
        if self.shrinks.is_empty() || self.shrinks.len() > MAX_TOOL_OUTPUT_PROJECTION_SHRINKS {
            bail!("tool-output projection sidecar shrink count is invalid");
        }
        let mut sources = BTreeSet::new();
        for shrink in &self.shrinks {
            shrink.validate_shape()?;
            if !sources.insert(shrink.source_event.event_id.clone()) {
                bail!("tool-output projection sidecar duplicates a source event");
            }
        }
        if let Some(transition) = &self.epoch_transition {
            transition.validate()?;
            if transition.reason == ToolOutputContextEpochTransitionReasonV1::SemanticCompaction
                && transition.target_epoch_id != format!("context-epoch:{}", self.compaction_id)
            {
                bail!("semantic tool-output epoch does not bind its compaction");
            }
            if transition.reason == ToolOutputContextEpochTransitionReasonV1::StandaloneShrink
                && (transition.target_epoch_id != self.compaction_id
                    || self.attempt_id
                        != format!("standalone-shrink-attempt:{}", self.compaction_id))
            {
                bail!("standalone tool-output epoch does not bind its durable identity");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct RecordedToolOutputProjection {
    outputs: Vec<ProjectedToolOutput>,
}

/// Read-only resolver for attempt-bound tool-output projection sidecars.
#[derive(Debug, Clone, Default)]
pub struct ToolOutputProjectionSidecarProjection {
    cursor: Option<ProjectionCursor>,
    by_compaction_id: BTreeMap<CompactionId, RecordedToolOutputProjection>,
    standalone_outputs: BTreeMap<EventId, ProjectedToolOutput>,
    latest_context_epoch_id: Option<String>,
}

impl ToolOutputProjectionSidecarProjection {
    /// Rebuilds all valid projection sidecars without mutating the durable stream.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed sources, stale plan cursors, bad causal lineage, duplicate
    /// compaction bindings, or descriptors that cannot be reproduced exactly from raw history.
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let mut projection = Self::default();
        for (index, record) in records.iter().enumerate() {
            projection.apply_record(records, index, record)?;
        }
        Ok(projection)
    }

    /// Returns the deterministic model-visible replacements bound to one active compaction.
    #[must_use]
    pub fn outputs_for_compaction(&self, compaction_id: &str) -> Option<&[ProjectedToolOutput]> {
        self.by_compaction_id
            .get(compaction_id)
            .map(|recorded| recorded.outputs.as_slice())
    }

    /// Returns all standalone replacements plus the active semantic compaction replacements,
    /// de-duplicated by immutable source event.
    #[must_use]
    pub fn active_outputs(&self, compaction_id: Option<&str>) -> Vec<ProjectedToolOutput> {
        let mut outputs = self.standalone_outputs.clone();
        if let Some(compaction_id) = compaction_id
            && let Some(recorded) = self.by_compaction_id.get(compaction_id)
        {
            for output in &recorded.outputs {
                outputs.insert(output.shrink.source_event.event_id.clone(), output.clone());
            }
        }
        outputs.into_values().collect()
    }

    /// Returns the latest explicit context epoch established by a shrink sidecar.
    ///
    /// Callers use this only as the source of a subsequent append-only standalone shrink. A
    /// semantic compaction without any shrink candidates has no shrink sidecar, so callers must
    /// still fall back to the active semantic compaction boundary (or the root epoch).
    #[must_use]
    pub fn latest_context_epoch_id(&self) -> Option<&str> {
        self.latest_context_epoch_id.as_deref()
    }

    /// Returns immutable source event ids that are already represented by an active standalone
    /// shrink. This prevents product previews from repeatedly offering the same historical output.
    #[must_use]
    pub fn active_standalone_source_event_ids(&self) -> BTreeSet<EventId> {
        self.standalone_outputs.keys().cloned().collect()
    }

    fn apply_record(
        &mut self,
        records: &[SessionStreamRecord],
        index: usize,
        record: &SessionStreamRecord,
    ) -> Result<()> {
        let event = record.stored_event();
        let decision = projection_apply_decision(self.cursor.as_ref(), event)?;
        if decision == crate::ProjectionApplyDecision::IgnoreAlreadyApplied {
            return Ok(());
        }
        decode_stored_event(event.clone())?;
        if event.event_kind() == Some(DurableEventType::ToolOutputProjectionShrinkRecorded) {
            let entry: ToolOutputProjectionShrinkRecorded =
                serde_json::from_value(event.payload.clone())
                    .context("failed to decode tool-output projection sidecar")?;
            let outputs = validate_recorded_sidecar(records, index, event, &entry)?;
            if self.by_compaction_id.contains_key(&entry.compaction_id) {
                bail!(
                    "tool-output projection sidecar for compaction {} was recorded more than once",
                    entry.compaction_id
                );
            }
            if entry.epoch_transition.as_ref().is_some_and(|transition| {
                transition.reason == ToolOutputContextEpochTransitionReasonV1::StandaloneShrink
            }) {
                for output in &outputs {
                    self.standalone_outputs
                        .insert(output.shrink.source_event.event_id.clone(), output.clone());
                }
            }
            if let Some(transition) = &entry.epoch_transition {
                self.latest_context_epoch_id = Some(transition.target_epoch_id.clone());
            }
            self.by_compaction_id.insert(
                entry.compaction_id,
                RecordedToolOutputProjection { outputs },
            );
        }
        self.cursor = Some(
            record.projection_cursor(TOOL_OUTPUT_PROJECTION_SIDECAR_PROJECTION_SCHEMA_VERSION),
        );
        Ok(())
    }
}

fn validate_recorded_sidecar(
    records: &[SessionStreamRecord],
    index: usize,
    event: &crate::StoredEvent,
    entry: &ToolOutputProjectionShrinkRecorded,
) -> Result<Vec<ProjectedToolOutput>> {
    entry.validate_shape()?;
    if entry.source_plan_cursor.session_id != event.session_id {
        bail!("tool-output projection sidecar source plan session does not match event session");
    }
    let source_count = usize::try_from(entry.source_plan_cursor.last_applied_stream_sequence)
        .context("tool-output projection sidecar source plan cursor overflows usize")?;
    let standalone = entry.epoch_transition.as_ref().is_some_and(|transition| {
        transition.reason == ToolOutputContextEpochTransitionReasonV1::StandaloneShrink
    });
    if (standalone && source_count != index) || (!standalone && source_count >= index) {
        bail!("tool-output projection sidecar source plan is not at its required frontier");
    }
    let source_tail = records
        .get(source_count.saturating_sub(1))
        .context("tool-output projection sidecar source plan cursor is missing")?;
    if source_tail.projection_cursor(COMPACTION_FOLD_PLAN_SCHEMA_VERSION)
        != entry.source_plan_cursor
    {
        bail!("tool-output projection sidecar source plan cursor does not match raw history");
    }
    let source_records = &records[..source_count];
    let plan = if let Some(adaptive_tail) = &entry.adaptive_tail {
        CompactionFoldPlan::from_records_after_adaptive_tail(
            source_records,
            adaptive_tail.policy.clone(),
            adaptive_tail.exact_fit_limit_tokens,
            entry.prior_folded_through.as_ref(),
        )?
    } else {
        CompactionFoldPlan::from_records_after(
            source_records,
            entry.requested_tail_message_count,
            entry.prior_folded_through.as_ref(),
        )?
    };
    if plan.base_stream_cursor != entry.source_plan_cursor {
        bail!("tool-output projection sidecar source plan rebuilt to a different cursor");
    }
    if plan.adaptive_tail != entry.adaptive_tail {
        bail!("tool-output projection sidecar adaptive tail proof does not match raw history");
    }

    if standalone {
        if event.correlation_id.as_deref() != Some(event.event_id.as_str())
            || event.causation_id.is_some()
        {
            bail!("standalone tool-output projection does not bind its epoch frontier");
        }
    } else {
        let lifecycle = CompactionLifecycleProjection::from_records(&records[..index])?;
        let attempt = lifecycle.attempt(&entry.attempt_id).with_context(|| {
            format!(
                "tool-output projection attempt {} is missing",
                entry.attempt_id
            )
        })?;
        let CompactionAttemptTerminal::Applied {
            event_id: applied_event_id,
            entry: applied,
            ..
        } = attempt
            .terminal
            .as_ref()
            .context("tool-output projection attempt is not terminal")?
        else {
            bail!("tool-output projection sidecar requires an applied compaction");
        };
        let planned_folded_through = plan
            .folded_through
            .as_ref()
            .context("tool-output projection sidecar source plan has no foldable history")?;
        if applied.compaction_id != entry.compaction_id
            || applied.folded_through != *planned_folded_through
        {
            bail!("tool-output projection sidecar does not match its applied fold boundary");
        }
        if event.correlation_id.as_deref() != Some(attempt.started_event_id.as_str())
            || event.causation_id.as_deref() != Some(applied_event_id.as_str())
        {
            bail!("tool-output projection sidecar does not remain in applied compaction lineage");
        }
    }
    let mut expected = ToolOutputProjection::from_fold_plan(source_records, &plan, &entry.policy)?;
    if standalone {
        let active_sources = ToolOutputProjectionSidecarProjection::from_records(source_records)?
            .active_standalone_source_event_ids();
        expected
            .outputs
            .retain(|output| !active_sources.contains(&output.shrink.source_event.event_id));
    }
    let expected_shrinks = expected
        .outputs
        .iter()
        .map(|output| output.shrink.clone())
        .collect::<Vec<_>>();
    if !projection_shrinks_match(&expected_shrinks, &entry.shrinks) {
        bail!("tool-output projection sidecar descriptors do not match raw source outputs");
    }
    Ok(expected.outputs)
}

fn projection_shrinks_match(
    expected: &[ToolOutputProjectionShrink],
    recorded: &[ToolOutputProjectionShrink],
) -> bool {
    expected.len() == recorded.len()
        && expected.iter().zip(recorded).all(|(expected, recorded)| {
            if expected == recorded {
                return true;
            }
            recorded.schema_version == 1
                && expected.source_event == recorded.source_event
                && expected.tool_call_id == recorded.tool_call_id
                && expected.source_message_sha256 == recorded.source_message_sha256
                && expected.original_content_bytes == recorded.original_content_bytes
                && expected.source_ref == recorded.source_ref
        })
}

impl JsonlSessionStore {
    /// Appends one standalone tool-output projection and explicitly rotates its context epoch.
    ///
    /// This does not publish a semantic checkpoint or fold boundary. Raw transcript events remain
    /// unchanged and replayable; only the provider-visible projection substitutes the bounded,
    /// recoverable tool-result representation.
    ///
    /// # Errors
    ///
    /// Returns an error when the plan is stale, the epoch transition is invalid, selected tool
    /// results are not old complete history, or the append frontier changed.
    pub fn append_standalone_tool_output_projection(
        &self,
        source_epoch_id: impl Into<String>,
        target_epoch_id: impl Into<String>,
        plan: CompactionFoldPlan,
        policy: ToolOutputProjectionPolicy,
    ) -> Result<Option<StoredEvent>> {
        let source_epoch_id = source_epoch_id.into();
        let target_epoch_id = target_epoch_id.into();
        let session_id = compaction_session_id(self)?;
        let source_records = self.read_event_records_writer()?;
        plan.validate_against(&source_records)?;
        let active_sources = ToolOutputProjectionSidecarProjection::from_records(&source_records)?
            .active_standalone_source_event_ids();
        let mut projection = ToolOutputProjection::from_fold_plan(&source_records, &plan, &policy)?;
        projection
            .outputs
            .retain(|output| !active_sources.contains(&output.shrink.source_event.event_id));
        if projection.outputs.is_empty() {
            return Ok(None);
        }
        let entry = ToolOutputProjectionShrinkRecorded::standalone(
            source_epoch_id,
            target_epoch_id.clone(),
            &plan,
            policy,
            &projection,
        )?;
        let event_id = crate::stable_event_uuid(
            "sigil-standalone-tool-output-projection",
            &format!(
                "{session_id}:{}:{target_epoch_id}",
                plan.base_stream_cursor.last_applied_event_id
            ),
        );
        let payload = serde_json::to_value(&entry)
            .context("failed to encode standalone tool-output projection")?;
        // A standalone shrink starts its own durable correlation chain. Its typed source cursor
        // binds the exact prior frontier; it cannot use causation because ordinary transcript
        // events intentionally have no correlation chain.
        let correlation_id = event_id.clone();
        let appended = self.append_event_if_with_identity(
            DurableEventType::ToolOutputProjectionShrinkRecorded,
            payload,
            event_id.clone(),
            Some(correlation_id.clone()),
            None,
            |records| {
                let existing = ToolOutputProjectionSidecarProjection::from_records(records)?;
                if existing.outputs_for_compaction(&target_epoch_id).is_some() {
                    return Ok(false);
                }
                let mut synthetic = crate::StoredEvent::new(
                    DurableEventType::ToolOutputProjectionShrinkRecorded,
                    EventClass::Critical,
                    event_id.clone(),
                    session_id.clone(),
                    next_stream_sequence(records),
                    serde_json::to_value(&entry)?,
                )?;
                synthetic.correlation_id = Some(correlation_id.clone());
                synthetic.causation_id = None;
                synthetic.record_checksum = synthetic.compute_record_checksum()?;
                validate_recorded_sidecar(records, records.len(), &synthetic, &entry)?;
                Ok(true)
            },
        )?;
        Ok(appended)
    }

    /// Appends one deterministic projection sidecar after its matching V2 compaction applied.
    ///
    /// The portable semantic-compaction executor calls this only after Started/Applied have
    /// committed under the same guarded lifecycle. This method preserves the separate durable
    /// invariant and idempotent writer boundary for replay and crash recovery.
    pub fn append_tool_output_projection_shrink_recorded(
        &self,
        entry: ToolOutputProjectionShrinkRecorded,
    ) -> Result<StoredEvent> {
        entry.validate_shape()?;
        let session_id = compaction_session_id(self)?;
        let records = self.read_event_records_writer()?;
        let lifecycle = CompactionLifecycleProjection::from_records(&records)?;
        let attempt = lifecycle.attempt(&entry.attempt_id).with_context(|| {
            format!(
                "tool-output projection attempt {} is missing",
                entry.attempt_id
            )
        })?;
        let CompactionAttemptTerminal::Applied {
            event_id: applied_event_id,
            entry: applied,
            ..
        } = attempt
            .terminal
            .as_ref()
            .context("tool-output projection attempt is not terminal")?
        else {
            bail!("tool-output projection sidecar requires an applied compaction");
        };
        if applied.compaction_id != entry.compaction_id {
            bail!("tool-output projection sidecar compaction id does not match its attempt");
        }
        let started_event_id = compaction_started_event_id(self, &entry.attempt_id)?;
        let sidecar_event_id = compaction_lifecycle_event_id(
            &session_id,
            &entry.attempt_id,
            "tool-output-projection-shrink",
        );
        let payload = serde_json::to_value(&entry)
            .context("failed to encode tool-output projection sidecar")?;
        let appended = self.append_event_if_with_identity(
            DurableEventType::ToolOutputProjectionShrinkRecorded,
            payload,
            sidecar_event_id,
            Some(started_event_id.clone()),
            Some(applied_event_id.clone()),
            |records| {
                let existing = ToolOutputProjectionSidecarProjection::from_records(records)?;
                if existing
                    .outputs_for_compaction(&entry.compaction_id)
                    .is_some()
                {
                    return Ok(false);
                }
                // Validate the candidate against the current records; append-time lineage uses
                // the same Applied event id captured above, and the reader rechecks it later.
                let mut synthetic = crate::StoredEvent::new(
                    DurableEventType::ToolOutputProjectionShrinkRecorded,
                    EventClass::Critical,
                    "pending-tool-output-projection".to_owned(),
                    session_id.clone(),
                    next_stream_sequence(records),
                    serde_json::to_value(&entry)?,
                )?;
                synthetic.correlation_id = Some(started_event_id.clone());
                synthetic.causation_id = Some(applied_event_id.clone());
                synthetic.record_checksum = synthetic.compute_record_checksum()?;
                validate_recorded_sidecar(records, records.len(), &synthetic, &entry)?;
                Ok(true)
            },
        )?;
        appended.context("tool-output projection sidecar append was not attempted")
    }
}

#[cfg(test)]
#[path = "tests/compaction_shrink_sidecar_tests.rs"]
mod tests;
