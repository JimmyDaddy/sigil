//! Exact, body-free historical facts lookup through the active session's original owner.
use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};

use super::*;

/// Selected original V3 facts. Artifact contents and transcript text are never returned.
#[derive(Debug, Clone)]
pub struct ArchivedToolResultFacts {
    pub facts: ToolResultFactsV1,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy)]
struct SourceOffset {
    offset: u64,
    sequence: u64,
}

/// Offsets only, rebuilt once per Session and extended from its original stream tail.
/// Values are reread and authenticated on every lookup; this never caches evidence bodies.
#[derive(Debug, Default)]
pub(super) struct ArchivedFactSources {
    results: BTreeMap<String, (SourceOffset, Option<SourceOffset>)>,
    pending_calls: BTreeMap<String, SourceOffset>,
    end_offset: u64,
    sequence: u64,
}

impl Session {
    /// Reads at most 32 retired results through this session's existing coordinated owner.
    /// Each binding must still belong to the active archive projection. The original result
    /// and declaring assistant are checked against its exact source and logical run identity.
    /// A missing store returns `None` for each source; malformed, missing or conflicting durable
    /// sources return an error rather than an empty successful history.
    ///
    /// # Errors
    /// Returns an error for stale/foreign bindings, corrupt sources, reader failures or limits.
    pub async fn archived_tool_result_facts(
        &self,
        bindings: &[ToolOutputArchivedArtifactBindingV1],
    ) -> Result<Vec<Option<ArchivedToolResultFacts>>> {
        ensure!(
            bindings.len() <= 32,
            "archived facts selection exceeds its bound"
        );
        if bindings.is_empty() {
            return Ok(Vec::new());
        }
        let Some(store) = self.store.as_ref() else {
            return Ok(vec![None; bindings.len()]);
        };
        let active = store.active_projection_snapshot()?;
        let pressure = active.tool_output_pressure();
        for binding in bindings {
            ensure!(
                pressure
                    .archived_artifact_bindings
                    .get(&binding.artifact_ref.artifact_id)
                    == Some(binding),
                "archived facts binding is not the active owner's exact source"
            );
        }
        let reader = store.read_handle();
        let session_id = active.frontier().session_id().to_owned();
        let bindings = bindings.to_vec();
        let sources = Arc::clone(&self.archived_fact_sources);
        let budget = SessionReadBudget::default();
        let cancel_on_drop = CancelArchiveReadOnDrop(budget.clone());
        let result = tokio::task::spawn_blocking(move || {
            let mut sources = loop {
                budget.check()?;
                match sources.try_lock() {
                    Ok(sources) => break sources,
                    Err(std::sync::TryLockError::WouldBlock) => {
                        std::thread::sleep(std::time::Duration::from_millis(5))
                    }
                    Err(std::sync::TryLockError::Poisoned(_)) => {
                        bail!("archived facts source index is poisoned")
                    }
                }
            };
            recover_archived_facts(&reader, &session_id, &bindings, &mut sources, &budget)
        })
        .await
        .context("archived facts lookup worker failed")?;
        drop(cancel_on_drop);
        result
    }
}

// Dropping an awaiting agent future stops the blocking reader at its next bounded I/O check.
struct CancelArchiveReadOnDrop(SessionReadBudget);
impl Drop for CancelArchiveReadOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn recover_archived_facts(
    reader: &SessionRecordReadHandle,
    session_id: &str,
    bindings: &[ToolOutputArchivedArtifactBindingV1],
    sources: &mut ArchivedFactSources,
    budget: &SessionReadBudget,
) -> Result<Vec<Option<ArchivedToolResultFacts>>> {
    let last_needed = bindings
        .iter()
        .map(|binding| binding.source_stream_sequence)
        .max()
        .expect("nonempty selection");
    while sources.sequence < last_needed {
        let range = reader.read_event_record_range_with_budget(
            sources.end_offset,
            sources.sequence,
            Some(session_id),
            256,
            8 * super::store::MAX_SESSION_RAW_RECORD_BYTES,
            budget,
        )?;
        ensure!(
            !range.records().is_empty(),
            "archived facts source is missing from its durable stream"
        );
        for ((record, offset), end_offset) in range
            .records()
            .iter()
            .zip(range.record_offsets())
            .zip(range.record_end_offsets())
        {
            budget.check()?;
            let event = record.stored_event();
            let source = SourceOffset {
                offset: *offset,
                sequence: event.stream_sequence,
            };
            match record.session_log_entry()? {
                Some(SessionLogEntry::Assistant(message)) => {
                    for call in message.tool_calls {
                        ensure!(
                            sources.pending_calls.len() < TOOL_OUTPUT_OPEN_CALL_MAX,
                            "archived facts exceeded its pending call limit"
                        );
                        ensure!(
                            sources.pending_calls.insert(call.id, source).is_none(),
                            "archived facts found conflicting call declarations"
                        );
                    }
                }
                Some(SessionLogEntry::ToolResultV3(result)) => {
                    ensure!(
                        sources.results.len() < TOOL_OUTPUT_PRESSURE_HARD_MAX_RESULTS,
                        "archived facts exceeded its source index limit"
                    );
                    let declaration = sources.pending_calls.remove(&result.call_id);
                    ensure!(
                        sources
                            .results
                            .insert(event.event_id.clone(), (source, declaration))
                            .is_none(),
                        "archived facts found conflicting source identities"
                    );
                }
                _ => {}
            }
            sources.end_offset = *end_offset;
            sources.sequence = event.stream_sequence;
        }
        sources.end_offset = range.end_offset();
        sources.sequence = range
            .records()
            .last()
            .expect("nonempty range")
            .stored_event()
            .stream_sequence;
    }
    bindings
        .iter()
        .map(|binding| {
            let (source, declaration) = sources
                .results
                .get(&binding.source_event_id)
                .context("archived facts source event is missing")?;
            ensure!(
                source.sequence == binding.source_stream_sequence,
                "archived facts source sequence conflicts with its binding"
            );
            let record = read_source(reader, session_id, *source, budget)?;
            ensure!(
                record.stored_event().event_id == binding.source_event_id,
                "archived facts source event conflicts with its binding"
            );
            let Some(SessionLogEntry::ToolResultV3(result)) = record.session_log_entry()? else {
                bail!("archived facts source is not an original V3 result");
            };
            result.validate()?;
            ensure!(
                result.message_id == binding.source_message_id
                    && result.call_id == binding.call_id
                    && result.tool_name == binding.tool_name,
                "archived facts result identity conflicts with its binding"
            );
            let ToolArtifactBindingV1::Published { descriptor } = &result.artifact else {
                bail!("archived facts source has no published artifact binding");
            };
            ensure!(
                descriptor.artifact_ref == binding.artifact_ref
                    && descriptor.content_sha256 == binding.artifact_sha256
                    && descriptor.persisted_bytes == binding.persisted_bytes,
                "archived facts artifact identity conflicts with its binding"
            );
            match declaration {
                Some(declaration) => {
                    let record = read_source(reader, session_id, *declaration, budget)?;
                    let Some(SessionLogEntry::Assistant(message)) = record.session_log_entry()?
                    else {
                        bail!("archived facts declaration is not an assistant batch");
                    };
                    ensure!(
                        message.logical_run_id == binding.logical_run_id
                            && message
                                .tool_calls
                                .iter()
                                .any(|call| call.id == result.call_id
                                    && call.name == result.tool_name),
                        "archived facts original logical run or call binding conflicts"
                    );
                }
                None => ensure!(
                    binding.logical_run_id.is_none(),
                    "archived facts logical run has no original declaration"
                ),
            }
            Ok(Some(ArchivedToolResultFacts {
                complete: matches!(descriptor.completeness, ToolArtifactCompleteness::Complete),
                facts: result.facts,
            }))
        })
        .collect()
}

fn read_source(
    reader: &SessionRecordReadHandle,
    session_id: &str,
    source: SourceOffset,
    budget: &SessionReadBudget,
) -> Result<SessionStreamRecord> {
    let range = reader.read_event_record_range_with_budget(
        source.offset,
        source.sequence.saturating_sub(1),
        Some(session_id),
        1,
        super::store::MAX_SESSION_RAW_RECORD_BYTES,
        budget,
    )?;
    range
        .into_records()
        .into_iter()
        .next()
        .context("archived facts source record is unavailable")
}
