use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::compaction_v2::{
    CompactionAttemptTerminal, CompactionLifecycleProjection, compaction_lifecycle_event_id,
    compaction_session_id, compaction_started_event_id,
};
use super::*;
use crate::{ProjectionCursor, projection_apply_decision};

/// Current schema for an attempt-bound, durable tool-output projection sidecar.
pub const TOOL_OUTPUT_PROJECTION_SIDECAR_SCHEMA_VERSION: u16 = 1;
/// Read-only projection cursor schema for tool-output shrink sidecars.
pub const TOOL_OUTPUT_PROJECTION_SIDECAR_PROJECTION_SCHEMA_VERSION: u16 = 1;

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
    /// Previous activated boundary used while planning a repeated compaction, if any.
    pub prior_folded_through: Option<super::compaction_v2::CompactionCursor>,
    pub policy: ToolOutputProjectionPolicy,
    pub shrinks: Vec<ToolOutputProjectionShrink>,
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
        let entry = Self {
            schema_version: TOOL_OUTPUT_PROJECTION_SIDECAR_SCHEMA_VERSION,
            compaction_id: compaction_id.into(),
            attempt_id: attempt_id.into(),
            source_plan_cursor: plan.base_stream_cursor.clone(),
            requested_tail_message_count: plan.requested_tail_message_count,
            prior_folded_through: plan.prior_folded_through.clone(),
            policy,
            shrinks: projection
                .outputs
                .iter()
                .map(|output| output.shrink.clone())
                .collect(),
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
    if source_count >= index {
        bail!("tool-output projection sidecar source plan must precede its own record");
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
    let plan = CompactionFoldPlan::from_records_after(
        source_records,
        entry.requested_tail_message_count,
        entry.prior_folded_through.as_ref(),
    )?;
    if plan.base_stream_cursor != entry.source_plan_cursor {
        bail!("tool-output projection sidecar source plan rebuilt to a different cursor");
    }

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
    let expected = ToolOutputProjection::from_fold_plan(source_records, &plan, &entry.policy)?;
    let expected_shrinks = expected
        .outputs
        .iter()
        .map(|output| output.shrink.clone())
        .collect::<Vec<_>>();
    if expected_shrinks != entry.shrinks {
        bail!("tool-output projection sidecar descriptors do not match raw source outputs");
    }
    Ok(expected.outputs)
}

impl JsonlSessionStore {
    /// Appends one deterministic projection sidecar after its matching V2 compaction applied.
    ///
    /// No ordinary flow calls this yet. K25.9 will create the plan, Started/Applied lifecycle and
    /// this sidecar as one guarded semantic-compaction sequence; this method only establishes the
    /// durable invariant and idempotent writer boundary.
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
