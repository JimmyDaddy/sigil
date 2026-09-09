//! Metadata-only display history. Source payloads are hydrated only after a page is selected.

use std::{cell::RefCell, sync::Arc};

use super::*;

mod inputs;
mod plan;
mod rows;
mod surfaces;
mod task;

use inputs::{InputMetadataProjection, InputSnapshot};
use plan::{PlanMetadataProjection, PlanSnapshot};
use rows::{RowProjectionState, RowSourceContext};
use surfaces::{RowCache, SurfaceBody, SurfaceCache};
use task::{TaskMetadataProjection, TaskSnapshot};

const MAX_CONVERSATION_DISPLAY_HYDRATION_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CONVERSATION_DISPLAY_RESPONSE_BYTES: usize = 1024 * 1024;

/// Exact committed source location supplied by the validated projection reader.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConversationDisplayRecordPosition {
    pub offset: u64,
    pub end: u64,
    pub sequence: u64,
    pub checksum: String,
}

#[derive(Debug)]
struct EnvelopeMetadata {
    prefix_digest: [u8; 32],
    position: ConversationDisplayRecordPosition,
}

#[derive(Debug, Clone)]
struct RowMetadata {
    order: ConversationDisplayOrderV1,
    projected_bytes: usize,
    source: ConversationDisplayRecordPosition,
    context: RowSourceContext,
}

/// Incremental canonical display index owned by one session projection owner.
#[derive(Debug, Default)]
pub struct ConversationDisplayIndex {
    scope: Option<String>,
    envelopes: Vec<EnvelopeMetadata>,
    rows: Vec<RowMetadata>,
    row_state: RowProjectionState,
    terminals: Vec<(u64, Option<ConversationTerminalFrontierV1>)>,
    task: TaskMetadataProjection,
    plan: PlanMetadataProjection,
    inputs: InputMetadataProjection,
    row_cache: RowCache,
    surface_cache: SurfaceCache,
}

/// Metadata hashing work retained for bounded-query qualification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversationDisplayHashMetrics {
    pub prefix_records_hashed: u64,
}

/// A fixed-frontier page plus the exact bounded source records needed to render it.
#[derive(Debug)]
pub struct ConversationDisplayPagePlan {
    scope: String,
    through_sequence: u64,
    terminal: Option<ConversationTerminalFrontierV1>,
    total_items: u64,
    rows: Vec<RowMetadata>,
    positions: Vec<ConversationDisplayRecordPosition>,
    next_cursor: Option<String>,
    has_more: bool,
    task: Option<TaskSnapshot>,
    plan: Option<PlanSnapshot>,
    inputs: Vec<InputSnapshot>,
    workspace: Option<String>,
    cached_rows: BTreeMap<ConversationDisplayOrderV1, Arc<ConversationDisplayItemV1>>,
    cached_surfaces: BTreeMap<u64, Arc<SurfaceBody>>,
}

impl ConversationDisplayIndex {
    /// Current cursors read one prefix digest without rehashing historical metadata.
    #[must_use]
    pub fn hash_metrics(&self) -> ConversationDisplayHashMetrics {
        ConversationDisplayHashMetrics {
            prefix_records_hashed: self.envelopes.len() as u64,
        }
    }

    /// Admit each newly validated durable record exactly once, in stream order.
    pub fn apply_record(
        &mut self,
        record: &SessionStreamRecord,
        position: ConversationDisplayRecordPosition,
    ) -> Result<()> {
        let previous = self.envelopes.last().map(|envelope| &envelope.position);
        let next_sequence = previous
            .map_or(0, |position| position.sequence)
            .checked_add(1)
            .context("conversation display stream sequence overflow")?;
        if position.sequence != next_sequence
            || position.sequence != record.stream_sequence()
            || position.checksum != record.record_checksum()
            || position.end <= position.offset
            || previous.is_some_and(|previous| previous.end > position.offset)
        {
            bail!("conversation display source position is not the next committed record");
        }
        if self
            .scope
            .as_deref()
            .is_some_and(|scope| scope != record.session_id())
        {
            bail!("conversation display session scope mismatch");
        }
        record.stored_event().verify_record_checksum()?;
        let scope = self
            .scope
            .get_or_insert_with(|| record.session_id().to_owned());
        let context = self.row_state.context(record)?;
        if let Some(entry) = record.session_log_entry()? {
            self.surface_cache.apply(&entry, position.sequence)?;
            let input = self.task.apply(&entry, &position)?;
            self.plan.apply(&entry, &position)?;
            self.inputs.apply(&entry, &position, input)?;
        }
        let mut projected = self.row_state.apply(record, scope)?;
        projected.sort_by_key(|item| item.display_order);
        for item in projected {
            let projected_bytes = serde_json::to_vec(&item)?.len();
            self.rows.push(RowMetadata {
                order: item.display_order,
                projected_bytes,
                source: position.clone(),
                context: context.clone(),
            });
            self.row_cache.apply(item, projected_bytes);
        }
        push_version(
            &mut self.terminals,
            position.sequence,
            self.row_state.terminal_frontier.clone(),
        );
        let previous_prefix = self
            .envelopes
            .last()
            .map_or([0_u8; 32], |envelope| envelope.prefix_digest);
        let prefix_digest = advance_display_prefix_digest(
            &previous_prefix,
            position.sequence,
            record.event_id(),
            record.record_checksum(),
        );
        self.envelopes.push(EnvelopeMetadata {
            prefix_digest,
            position,
        });
        Ok(())
    }

    /// Select rows and summary sources using metadata; this method performs no payload reads.
    pub fn prepare_page(
        &self,
        expected_scope: &str,
        cursor: Option<&str>,
        limit: usize,
        workspace: Option<&str>,
    ) -> std::result::Result<ConversationDisplayPagePlan, ConversationDisplayProjectionError> {
        validate_page_request(expected_scope, limit)?;
        if self
            .scope
            .as_deref()
            .is_some_and(|scope| scope != expected_scope)
        {
            return Err(anyhow!("conversation display session scope mismatch").into());
        }
        let cursor = cursor
            .map(decode_cursor)
            .transpose()
            .map_err(ConversationDisplayProjectionError::invalid_cursor)?;
        if let Some(cursor) = &cursor {
            validate_cursor_request(cursor, expected_scope)
                .map_err(ConversationDisplayProjectionError::invalid_cursor)?;
        }
        let latest = self
            .envelopes
            .last()
            .map_or(0, |envelope| envelope.position.sequence);
        let through_sequence = cursor
            .as_ref()
            .map_or(latest, |cursor| cursor.through_session_stream_sequence);
        if let Some(cursor) = &cursor {
            if through_sequence == 0
                || through_sequence > latest
                || self.frontier_hash(expected_scope, through_sequence, cursor.before_order)
                    != cursor.frontier_binding_sha256
            {
                return Err(ConversationDisplayProjectionError::stale_cursor(anyhow!(
                    "conversation display cursor frontier no longer matches durable history"
                )));
            }
            if self
                .rows
                .binary_search_by_key(&cursor.before_order, |row| row.order)
                .is_err()
            {
                return Err(ConversationDisplayProjectionError::stale_cursor(anyhow!(
                    "conversation display cursor boundary is not a projected item"
                )));
            }
        }
        let total = self
            .rows
            .partition_point(|row| row.order.session_stream_sequence <= through_sequence);
        let eligible = cursor.as_ref().map_or(total, |cursor| {
            self.rows[..total].partition_point(|row| row.order < cursor.before_order)
        });
        let task = self.task.at(through_sequence);
        let plan = self.plan.at(through_sequence);
        let inputs = self.inputs.at(through_sequence);
        let mut positions = BTreeMap::new();
        let mut cached_surfaces = BTreeMap::new();
        for position in task
            .iter()
            .flat_map(TaskSnapshot::positions)
            .chain(plan.iter().flat_map(PlanSnapshot::positions))
            .chain(inputs.iter().map(InputSnapshot::position))
        {
            if let Some(surface) = self.surface_cache.get(position.sequence) {
                cached_surfaces.insert(position.sequence, surface);
            } else {
                positions.insert(position.sequence, position.clone());
            }
        }
        let mut hydrated_bytes = positions.values().try_fold(0_u64, |bytes, position| {
            bytes
                .checked_add(position.end - position.offset)
                .context("conversation display source byte count overflow")
        })?;
        if hydrated_bytes > MAX_CONVERSATION_DISPLAY_HYDRATION_BYTES {
            return Err(anyhow!(
                "conversation display summary sources exceed the 4 MiB hydration budget"
            )
            .into());
        }
        let mut rows = Vec::with_capacity(limit);
        let mut cached_rows = BTreeMap::new();
        let mut bytes = 0_usize;
        for row in self.rows[..eligible].iter().rev().take(limit) {
            if !rows.is_empty()
                && bytes.saturating_add(row.projected_bytes) > MAX_CONVERSATION_DISPLAY_PAGE_BYTES
            {
                break;
            }
            let cached = self.row_cache.get(row.order);
            let raw_bytes = if cached.is_none() && !positions.contains_key(&row.source.sequence) {
                row.source.end - row.source.offset
            } else {
                0
            };
            if hydrated_bytes.saturating_add(raw_bytes) > MAX_CONVERSATION_DISPLAY_HYDRATION_BYTES {
                if rows.is_empty() {
                    return Err(anyhow!("conversation display row source exceeds the remaining 4 MiB hydration budget").into());
                }
                break;
            }
            if let Some(item) = cached {
                cached_rows.insert(row.order, item);
            } else {
                positions.insert(row.source.sequence, row.source.clone());
            }
            hydrated_bytes += raw_bytes;
            bytes = bytes.saturating_add(row.projected_bytes);
            rows.push(row.clone());
        }
        rows.reverse();
        let has_more = eligible > rows.len();
        let next_cursor = if has_more {
            let before_order = rows
                .first()
                .context("bounded display page could not retain one item")?
                .order;
            Some(encode_cursor(&ConversationDisplayCursor {
                schema_version: CONVERSATION_DISPLAY_CURSOR_SCHEMA_VERSION,
                session_scope_sha256: scope_sha256(expected_scope),
                through_session_stream_sequence: through_sequence,
                frontier_binding_sha256: self.frontier_hash(
                    expected_scope,
                    through_sequence,
                    before_order,
                ),
                before_order,
            })?)
        } else {
            None
        };
        Ok(ConversationDisplayPagePlan {
            scope: expected_scope.to_owned(),
            through_sequence,
            terminal: version_at(&self.terminals, through_sequence)
                .cloned()
                .flatten(),
            total_items: u64::try_from(total)
                .context("conversation display item count overflow")?,
            rows,
            positions: positions.into_values().collect(),
            next_cursor,
            has_more,
            task,
            plan,
            inputs,
            workspace: workspace.map(str::to_owned),
            cached_rows,
            cached_surfaces,
        })
    }

    fn frontier_hash(
        &self,
        scope: &str,
        sequence: u64,
        before: ConversationDisplayOrderV1,
    ) -> String {
        let prefix = sequence
            .checked_sub(1)
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| self.envelopes.get(index))
            .map_or([0_u8; 32], |envelope| envelope.prefix_digest);
        display_cursor_binding_sha256(scope, sequence, before, &prefix)
    }
}

impl ConversationDisplayPagePlan {
    /// Deduplicated exact sources, in durable stream order.
    pub fn positions(&self) -> &[ConversationDisplayRecordPosition] {
        &self.positions
    }

    /// Verify and render the selected payloads after the owner charges their hydration budget.
    pub fn finish(
        self,
        records: &[SessionStreamRecord],
        artifact_store: Option<&ToolArtifactStore>,
    ) -> std::result::Result<ConversationDisplayPageV1, ConversationDisplayProjectionError> {
        let records =
            BodyRecords::new(&self.scope, &self.positions, records, self.cached_surfaces)?;
        let mut items = Vec::with_capacity(self.rows.len());
        let mut projected = BTreeMap::new();
        for row in self.rows {
            if let Some(item) = self.cached_rows.get(&row.order) {
                items.push(item.as_ref().clone());
                continue;
            }
            if let std::collections::btree_map::Entry::Vacant(entry) =
                projected.entry(row.source.sequence)
            {
                let source = records.record(&row.source)?;
                entry.insert(row.context.project(source, &self.scope)?);
            }
            let item = projected
                .get(&row.source.sequence)
                .and_then(|items| items.iter().find(|item| item.display_order == row.order))
                .context("conversation display source lost its selected row")?
                .clone();
            if serde_json::to_vec(&item)
                .context("failed to measure hydrated display row")?
                .len()
                != row.projected_bytes
            {
                return Err(
                    anyhow!("conversation display source changed its projected size").into(),
                );
            }
            items.push(item);
        }
        let user_inputs = stable_pending_user_inputs(
            self.inputs
                .into_iter()
                .map(|input| input.hydrate(&records))
                .collect::<Result<Vec<_>>>()?,
        );
        let mut page = ConversationDisplayPageV1 {
            schema_version: CONVERSATION_DISPLAY_SCHEMA_VERSION,
            session_scope_id: self.scope,
            through_session_stream_sequence: self.through_sequence,
            terminal_frontier: self.terminal,
            total_items: self.total_items,
            items,
            next_cursor: self.next_cursor,
            has_more: self.has_more,
            task_control: self.task.map(|task| task.hydrate(&records)).transpose()?,
            plan_review: self
                .plan
                .map(|plan| plan.hydrate(&records, self.workspace.as_deref()))
                .transpose()?,
            user_input: user_inputs.first().cloned(),
            user_inputs,
        };
        if let Some(store) = artifact_store {
            reconcile_physical_artifact_availability(&mut page, store);
        }
        if serde_json::to_vec(&page)
            .context("failed to measure conversation display response")?
            .len()
            > MAX_CONVERSATION_DISPLAY_RESPONSE_BYTES
        {
            return Err(
                anyhow!("conversation display response exceeds the 1 MiB output budget").into(),
            );
        }
        Ok(page)
    }
}

struct BodyRecords<'a> {
    records: BTreeMap<u64, &'a SessionStreamRecord>,
    surfaces: RefCell<BTreeMap<u64, Arc<SurfaceBody>>>,
}

impl<'a> BodyRecords<'a> {
    fn new(
        scope: &str,
        positions: &[ConversationDisplayRecordPosition],
        records: &'a [SessionStreamRecord],
        surfaces: BTreeMap<u64, Arc<SurfaceBody>>,
    ) -> Result<Self> {
        if positions.len() != records.len() {
            bail!("conversation display hydration source count mismatch");
        }
        let mut indexed = BTreeMap::new();
        for (position, record) in positions.iter().zip(records) {
            if record.session_id() != scope
                || record.stream_sequence() != position.sequence
                || record.record_checksum() != position.checksum
            {
                bail!("conversation display hydration source identity mismatch");
            }
            record.stored_event().verify_record_checksum()?;
            indexed.insert(position.sequence, record);
        }
        Ok(Self {
            records: indexed,
            surfaces: RefCell::new(surfaces),
        })
    }
    fn record(&self, position: &ConversationDisplayRecordPosition) -> Result<&SessionStreamRecord> {
        self.records
            .get(&position.sequence)
            .copied()
            .context("conversation display hydration source is missing")
    }
    fn surface(&self, position: &ConversationDisplayRecordPosition) -> Result<Arc<SurfaceBody>> {
        if let Some(surface) = self.surfaces.borrow().get(&position.sequence) {
            return Ok(Arc::clone(surface));
        }
        let entry = self
            .record(position)?
            .session_log_entry()?
            .context("conversation display summary source lost its entry")?;
        let surface = Arc::new(
            SurfaceBody::from_entry(&entry)
                .context("conversation display summary source changed type")?,
        );
        self.surfaces
            .borrow_mut()
            .insert(position.sequence, Arc::clone(&surface));
        Ok(surface)
    }
}

fn push_version<T: PartialEq>(versions: &mut Vec<(u64, T)>, sequence: u64, value: T) {
    if versions
        .last()
        .is_none_or(|(_, previous)| previous != &value)
    {
        versions.push((sequence, value));
    }
}

fn version_at<T>(versions: &[(u64, T)], sequence: u64) -> Option<&T> {
    let end = versions.partition_point(|(version, _)| *version <= sequence);
    end.checked_sub(1).map(|index| &versions[index].1)
}
