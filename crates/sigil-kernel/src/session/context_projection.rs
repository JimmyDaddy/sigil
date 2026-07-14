use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};

use super::*;
use crate::EventId;

/// Schema version for the provider-neutral chat context projection.
pub const SESSION_CONTEXT_PROJECTION_SCHEMA_VERSION: u16 = 1;

/// One provider-visible message retained by a session context projection.
#[derive(Debug, Clone)]
pub struct SessionProjectionEntry {
    /// The exact provider-neutral chat message retained by this projection.
    pub message: ModelMessage,
}

/// Whether an active TaskMemory payload still matches the current workspace snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskMemorySnapshotRelation {
    /// The caller supplied a snapshot equal to the TaskMemory capture snapshot.
    Same,
    /// The caller supplied a snapshot that differs from the TaskMemory capture snapshot.
    Changed {
        captured: crate::WorkspaceSnapshotId,
        current: crate::WorkspaceSnapshotId,
    },
    /// No current workspace snapshot was available when the projection was built.
    CurrentUnknown,
}

/// Trust facts carried beside provider-visible history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextTrustProjection {
    /// Message ids whose durable provenance marks their contents external and untrusted.
    pub external_untrusted_message_ids: BTreeSet<String>,
}

/// Provider-neutral session context derived from append-only session state.
///
/// Before the first V2 activation, K25.5 retains the existing chat projection unchanged. A V2
/// `CompactionAppliedV2` then becomes the sole V2 boundary source, while K25.9 remains
/// responsible for adding model-visible checkpoint material before any folded raw history can be
/// removed.
#[derive(Debug, Clone)]
pub struct SessionContextProjection {
    /// Version of this projection shape.
    pub projection_schema_version: u16,
    /// The only durable event that may activate a V2 compaction boundary.
    pub active_compaction_id: Option<CompactionId>,
    /// Stable cursor for the activated V2 boundary.
    pub folded_through: Option<CompactionCursor>,
    /// Active TaskMemory sidecar, if the AppliedV2 event references one that has not been invalidated.
    pub task_memory: Option<TaskMemoryV1>,
    /// Relation between active TaskMemory and a caller-supplied current workspace snapshot.
    pub task_memory_snapshot_relation: Option<TaskMemorySnapshotRelation>,
    /// Checkpoint binding paired with the active TaskMemory sidecar.
    pub checkpoint: Option<ContinuationCheckpointV1>,
    /// Provider-visible history retained by this projection.
    pub retained_entries: Vec<SessionProjectionEntry>,
    /// Trust classification for retained durable message sources.
    pub trust_projection: ContextTrustProjection,
}

impl SessionContextProjection {
    pub(crate) fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let raw_messages = raw_model_messages(entries);
        let retained_messages = repair_orphan_tool_results(&raw_messages);
        Self {
            projection_schema_version: SESSION_CONTEXT_PROJECTION_SCHEMA_VERSION,
            active_compaction_id: None,
            folded_through: None,
            task_memory: None,
            task_memory_snapshot_relation: None,
            checkpoint: None,
            retained_entries: into_projection_entries(retained_messages),
            trust_projection: trust_projection(entries),
        }
    }

    pub(crate) fn from_durable_records(
        entries: &[SessionLogEntry],
        records: &[SessionStreamRecord],
        branch_id: Option<&str>,
    ) -> Result<Self> {
        let mut projection = Self::from_entries(entries);
        let lifecycle = CompactionLifecycleProjection::from_records(records)?;
        let sidecars = CompactionSidecarProjection::from_records(records)?;
        let output_sidecars = ToolOutputProjectionSidecarProjection::from_records(records)?;
        let activated = lifecycle
            .attempts()
            .filter_map(|attempt| {
                let CompactionAttemptTerminal::Applied {
                    stream_sequence,
                    entry,
                    ..
                } = attempt.terminal.as_ref()?
                else {
                    return None;
                };
                let entry = entry.as_ref();
                if entry.branch_id.as_deref() != branch_id {
                    return None;
                }
                let sidecar = match entry.task_memory_id.as_deref() {
                    Some(_) => sidecars
                        .resolved_compaction(&entry.compaction_id)
                        .cloned()?,
                    None => return Some((*stream_sequence, entry.clone(), None)),
                };
                Some((*stream_sequence, entry.clone(), Some(sidecar)))
            })
            .max_by_key(|(stream_sequence, _, _)| *stream_sequence);

        if let Some((_, applied, sidecar)) = activated {
            let outputs = output_sidecars
                .outputs_for_compaction(&applied.compaction_id)
                .unwrap_or_default();
            let raw_messages = raw_model_messages_from_durable_records(records, outputs)?;
            let portable_retained_raw_event_ids = (sidecar.is_some()
                && applied.checkpoint.kind == ContinuationCheckpointKind::PortableSemantic)
                .then(|| portable_retained_raw_event_ids(records, &applied))
                .transpose()?;
            projection.activate_v2_boundary(
                applied,
                sidecar,
                raw_messages,
                portable_retained_raw_event_ids.as_ref(),
            )?;
        }
        Ok(projection)
    }

    /// Returns the provider-visible messages derived from this single projection source.
    #[must_use]
    pub fn model_messages(&self) -> Vec<ModelMessage> {
        self.retained_entries
            .iter()
            .map(|entry| entry.message.clone())
            .collect()
    }

    /// Builds a process-local projection for the request that would follow a portable
    /// compaction activation.
    ///
    /// This is intentionally not an activation: it retains the caller's durable trust facts but
    /// does not claim an active compaction id or mutate the append-only session stream.
    pub(crate) fn with_portable_candidate(
        &self,
        checkpoint: &ContinuationCheckpointV1,
        task_memory: &TaskMemoryV1,
        candidate_messages: Vec<ModelMessage>,
    ) -> Result<Self> {
        checkpoint.render_for_provider(task_memory)?;
        Ok(Self {
            projection_schema_version: SESSION_CONTEXT_PROJECTION_SCHEMA_VERSION,
            active_compaction_id: None,
            folded_through: None,
            task_memory: Some(task_memory.clone()),
            task_memory_snapshot_relation: Some(TaskMemorySnapshotRelation::CurrentUnknown),
            checkpoint: Some(checkpoint.clone()),
            retained_entries: into_projection_entries(repair_orphan_tool_results(
                &candidate_messages,
            )),
            trust_projection: self.trust_projection.clone(),
        })
    }

    /// Sets the TaskMemory/workspace relation without changing its durable activation or messages.
    #[must_use]
    pub fn with_current_workspace_snapshot(
        mut self,
        current_snapshot: Option<crate::WorkspaceSnapshotId>,
    ) -> Self {
        let Some(memory) = &self.task_memory else {
            return self;
        };
        self.task_memory_snapshot_relation = Some(match current_snapshot {
            Some(current) if current == memory.valid_for_snapshot => {
                TaskMemorySnapshotRelation::Same
            }
            Some(current) => TaskMemorySnapshotRelation::Changed {
                captured: memory.valid_for_snapshot.clone(),
                current,
            },
            None => TaskMemorySnapshotRelation::CurrentUnknown,
        });
        self
    }

    fn activate_v2_boundary(
        &mut self,
        applied: CompactionAppliedV2,
        sidecar: Option<ResolvedCompactionSidecar>,
        raw_messages: Vec<DurableProjectionMessage>,
        portable_retained_raw_event_ids: Option<&BTreeSet<EventId>>,
    ) -> Result<()> {
        self.active_compaction_id = Some(applied.compaction_id);
        self.folded_through = Some(applied.folded_through);
        if let Some(sidecar) = sidecar {
            self.task_memory = Some(sidecar.task_memory);
            self.task_memory_snapshot_relation = Some(TaskMemorySnapshotRelation::CurrentUnknown);
            self.checkpoint = Some(sidecar.checkpoint);
            if self.checkpoint.as_ref().is_some_and(|checkpoint| {
                checkpoint.kind == ContinuationCheckpointKind::PortableSemantic
            }) {
                let task_memory = self
                    .task_memory
                    .as_ref()
                    .expect("task memory was set with the active portable checkpoint");
                let checkpoint_message = self
                    .checkpoint
                    .as_ref()
                    .expect("checkpoint was set with the active portable checkpoint")
                    .render_for_provider(task_memory)?;
                let mut retained_messages = Vec::with_capacity(raw_messages.len() + 1);
                retained_messages.push(checkpoint_message);
                retained_messages.extend(
                    raw_messages
                        .into_iter()
                        .filter(|message| {
                            portable_retained_raw_event_ids
                                .expect("portable activation rebuilt retained raw event ids")
                                .contains(&message.event_id)
                        })
                        .map(|message| message.message),
                );
                self.retained_entries =
                    into_projection_entries(repair_orphan_tool_results(&retained_messages));
            } else {
                self.retained_entries = into_projection_entries(repair_orphan_tool_results(
                    &raw_messages
                        .into_iter()
                        .map(|message| message.message)
                        .collect::<Vec<_>>(),
                ));
            }
        } else {
            // A V2 lifecycle record without an activated TaskMemory/checkpoint must not hide raw
            // history. Provider-native candidates gain their own resolver in K25.12.
            self.retained_entries = into_projection_entries(repair_orphan_tool_results(
                &raw_messages
                    .into_iter()
                    .map(|message| message.message)
                    .collect::<Vec<_>>(),
            ));
        }
        Ok(())
    }
}

pub(super) fn raw_model_messages(entries: &[SessionLogEntry]) -> Vec<ModelMessage> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::User(message)
            | SessionLogEntry::Assistant(message)
            | SessionLogEntry::ToolResult(message) => Some(message.clone()),
            SessionLogEntry::Control(_) => None,
        })
        .collect()
}

#[derive(Debug, Clone)]
struct DurableProjectionMessage {
    event_id: EventId,
    message: ModelMessage,
}

fn raw_model_messages_from_durable_records(
    records: &[SessionStreamRecord],
    outputs: &[ProjectedToolOutput],
) -> Result<Vec<DurableProjectionMessage>> {
    let replacements = outputs
        .iter()
        .map(|output| {
            (
                output.shrink.source_event.event_id.as_str(),
                output.message.clone(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    if replacements.len() != outputs.len() {
        bail!("tool-output projection sidecar contains duplicate source event ids");
    }
    let mut messages = Vec::new();
    for record in records {
        let event = record.stored_event();
        let Some(entry) = session_entry_from_stored_event(event)? else {
            continue;
        };
        let message = match entry {
            SessionLogEntry::User(message)
            | SessionLogEntry::Assistant(message)
            | SessionLogEntry::ToolResult(message) => replacements
                .get(event.event_id.as_str())
                .cloned()
                .unwrap_or(message),
            SessionLogEntry::Control(_) => continue,
        };
        messages.push(DurableProjectionMessage {
            event_id: event.event_id.clone(),
            message,
        });
    }
    Ok(messages)
}

/// Reconstructs the exact provider-visible history that a portable checkpoint would expose
/// after activation, without writing a lifecycle or sidecar record.
pub(crate) fn portable_candidate_model_messages(
    records: &[SessionStreamRecord],
    plan: &CompactionFoldPlan,
    checkpoint: &ContinuationCheckpointV1,
    task_memory: &TaskMemoryV1,
) -> Result<Vec<ModelMessage>> {
    let checkpoint_message = checkpoint.render_for_provider(task_memory)?;
    let raw_messages = raw_model_messages_from_durable_records(records, &[])?;
    let retained_raw_event_ids = portable_retained_raw_event_ids_for_plan(records, plan)?;
    let mut candidate = Vec::with_capacity(raw_messages.len().saturating_add(1));
    candidate.push(checkpoint_message);
    candidate.extend(
        raw_messages
            .into_iter()
            .filter(|message| retained_raw_event_ids.contains(&message.event_id))
            .map(|message| message.message),
    );
    Ok(repair_orphan_tool_results(&candidate))
}

/// Returns the raw provider-visible event ids that remain beside a portable checkpoint for one
/// exact fold plan. Messages protected by the current plan remain visible; only messages already
/// represented by a prior checkpoint or selected for this fold are hidden.
fn portable_retained_raw_event_ids_for_plan(
    records: &[SessionStreamRecord],
    plan: &CompactionFoldPlan,
) -> Result<BTreeSet<EventId>> {
    plan.validate_against(records)?;
    let folded = plan.folded_event_ids.iter().collect::<BTreeSet<_>>();
    let prior_boundary = plan
        .protected_events
        .iter()
        .filter(|protected| {
            protected.reason == CompactionFoldProtectionReason::ExistingCompactionBoundary
        })
        .map(|protected| &protected.event.event_id)
        .collect::<BTreeSet<_>>();

    let mut retained = BTreeSet::new();
    for record in records {
        let event = record.stored_event();
        let Some(entry) = session_entry_from_stored_event(event)? else {
            continue;
        };
        if matches!(
            entry,
            SessionLogEntry::User(_)
                | SessionLogEntry::Assistant(_)
                | SessionLogEntry::ToolResult(_)
        ) && !folded.contains(&event.event_id)
            && !prior_boundary.contains(&event.event_id)
        {
            retained.insert(event.event_id.clone());
        }
    }
    Ok(retained)
}

/// Reconstructs the retained raw event ids for an activated portable checkpoint and includes
/// provider-visible messages appended after the frozen source cursor. The reconstruction is
/// intentionally fail-closed: a tampered source plan cannot silently hide raw history.
fn portable_retained_raw_event_ids(
    records: &[SessionStreamRecord],
    applied: &CompactionAppliedV2,
) -> Result<BTreeSet<EventId>> {
    let checkpoint = &applied.checkpoint;
    let source_cursor = checkpoint
        .source_plan_cursor
        .as_ref()
        .context("portable checkpoint is missing its source plan cursor")?;
    let source_count = usize::try_from(source_cursor.last_applied_stream_sequence)
        .context("portable checkpoint source plan cursor overflows usize")?;
    let source_records = records
        .get(..source_count)
        .context("portable checkpoint source plan cursor is missing")?;
    let plan = CompactionFoldPlan::from_records_after(
        source_records,
        checkpoint
            .requested_tail_message_count
            .context("portable checkpoint is missing its requested tail size")?,
        checkpoint.prior_folded_through.as_ref(),
    )?;
    if plan.base_stream_cursor != *source_cursor
        || plan.folded_through.as_ref() != Some(&applied.folded_through)
    {
        bail!("portable checkpoint does not match its deterministic retained-history plan");
    }

    let mut retained = portable_retained_raw_event_ids_for_plan(source_records, &plan)?;
    for record in &records[source_count..] {
        let event = record.stored_event();
        if matches!(
            session_entry_from_stored_event(event)?,
            Some(
                SessionLogEntry::User(_)
                    | SessionLogEntry::Assistant(_)
                    | SessionLogEntry::ToolResult(_)
            )
        ) {
            retained.insert(event.event_id.clone());
        }
    }
    Ok(retained)
}

fn into_projection_entries(messages: Vec<ModelMessage>) -> Vec<SessionProjectionEntry> {
    messages
        .into_iter()
        .map(|message| SessionProjectionEntry { message })
        .collect()
}

fn trust_projection(entries: &[SessionLogEntry]) -> ContextTrustProjection {
    ContextTrustProjection {
        external_untrusted_message_ids: entries
            .iter()
            .filter_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::ExternalProvenance(provenance)) => {
                    Some(provenance.message_id.clone())
                }
                _ => None,
            })
            .collect(),
    }
}

#[cfg(test)]
#[path = "tests/context_projection_tests.rs"]
mod tests;
