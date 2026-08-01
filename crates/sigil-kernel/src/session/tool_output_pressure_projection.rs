use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    ActiveProjectionFrontier, ControlEntry, JsonlSessionStore, ProjectionCursor, SessionLogEntry,
    SessionStreamRecord, ToolArtifactAvailability, ToolArtifactBindingV1, ToolArtifactCompleteness,
    ToolArtifactGcRootsV1, ToolArtifactRefV1, ToolExecutionStatus, ToolModelViewV1,
    ToolPreviewKind, ToolResultFactsV1, session_entry_from_stored_event,
};
use crate::{
    DurableEventType, EventId, MutationCommitted, MutationPrepared, MutationSubject, StoredEvent,
    stable_event_hash, stable_event_uuid,
};

pub const TOOL_OUTPUT_PRESSURE_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const TOOL_OUTPUT_AGING_POLICY_VERSION: u16 = 1;
pub const TOOL_OUTPUT_AGING_ACTIVATION_SCHEMA_VERSION: u16 = 1;
pub const TOOL_OUTPUT_RECENT_PROTECTED_TOKENS: u64 = 32 * 1024;
pub const TOOL_OUTPUT_MIN_BATCH_RECLAIM_TOKENS: u64 = 16 * 1024;
pub const TOOL_OUTPUT_AGED_RESULT_TARGET_TOKENS: u64 = 1024;
pub const TOOL_OUTPUT_AGED_RESULT_MAX_BYTES: usize = 4 * 1024;
pub const TOOL_OUTPUT_AGING_MAX_RESULTS: usize = 128;
/// Preferred in-memory working-set size; only already-aged items may be retired at this boundary.
pub const TOOL_OUTPUT_PRESSURE_MAX_RESULTS: usize = 4096;
/// Absolute manifest-aligned guard against corrupt or adversarial unbounded session streams.
pub const TOOL_OUTPUT_PRESSURE_HARD_MAX_RESULTS: usize = 100_000;
pub const TOOL_OUTPUT_OPEN_CALL_MAX: usize = 1024;
const TOOL_OUTPUT_MUTATION_LINK_MAX: usize = TOOL_OUTPUT_PRESSURE_MAX_RESULTS * 16;
pub const TOOL_OUTPUT_ARCHIVED_ARTIFACT_BINDING_MAX: usize = 100_000;

#[derive(Debug, Clone, Default)]
struct ToolOutputSignalEnrichmentV1 {
    approval_receipt_refs: Vec<String>,
    mutation_receipt_refs: Vec<String>,
    verification_receipt_refs: Vec<String>,
    external_provenance_refs: Vec<String>,
    changed_files: Vec<String>,
    marks_error: bool,
}

impl ToolOutputSignalEnrichmentV1 {
    fn apply_to(self, facts: &mut ToolResultFactsV1) {
        if self.marks_error {
            facts.status = "error".to_owned();
        }
        for receipt_ref in self.approval_receipt_refs {
            facts.add_approval_receipt_ref(&receipt_ref);
        }
        for receipt_ref in self.mutation_receipt_refs {
            facts.add_mutation_receipt_ref(&receipt_ref);
        }
        for receipt_ref in self.verification_receipt_refs {
            facts.add_verification_receipt_ref(&receipt_ref);
        }
        for provenance_ref in self.external_provenance_refs {
            facts.add_external_provenance_ref(&provenance_ref);
        }
        for path in self.changed_files {
            facts.add_changed_file(&path);
        }
    }

    fn merge(&mut self, other: Self) {
        self.marks_error |= other.marks_error;
        merge_unique(&mut self.approval_receipt_refs, other.approval_receipt_refs);
        merge_unique(&mut self.mutation_receipt_refs, other.mutation_receipt_refs);
        merge_unique(
            &mut self.verification_receipt_refs,
            other.verification_receipt_refs,
        );
        merge_unique(
            &mut self.external_provenance_refs,
            other.external_provenance_refs,
        );
        merge_unique(&mut self.changed_files, other.changed_files);
    }
}

fn merge_unique(target: &mut Vec<String>, values: Vec<String>) {
    for value in values {
        if !target.contains(&value) {
            target.push(value);
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputRetentionClassV1 {
    UnpairedProtected,
    CurrentTurn,
    HighSignalProtected,
    RecentProtected,
    Ageable,
    Aged,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolOutputPressureItemV1 {
    pub source_event_id: String,
    pub source_stream_sequence: u64,
    pub message_id: String,
    pub call_id: String,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<ToolArtifactRefV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_sha256: Option<String>,
    pub artifact_availability: ToolArtifactAvailability,
    pub complete: bool,
    pub observed_bytes: u64,
    pub persisted_bytes: u64,
    pub initial_model_tokens: u64,
    pub current_model_tokens: u64,
    /// Bounded, policy-safe excerpt used by local cleanup review surfaces.
    pub preview_excerpt: String,
    pub high_signal: bool,
    pub pair_closed: bool,
    pub retention: ToolOutputRetentionClassV1,
    pub turn_index: u64,
    pub initial_model_view_sha256: String,
    pub facts: ToolResultFactsV1,
}

/// Minimal body-free source binding retained after an already-aged item leaves the working set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolOutputArchivedArtifactBindingV1 {
    pub artifact_ref: ToolArtifactRefV1,
    pub source_event_id: String,
    pub source_stream_sequence: u64,
    pub source_message_id: String,
    pub artifact_sha256: String,
    pub persisted_bytes: u64,
    pub call_id: String,
    pub tool_name: String,
    pub artifact_availability: ToolArtifactAvailability,
    pub archived: bool,
}

impl ToolOutputArchivedArtifactBindingV1 {
    fn validate(&self) -> Result<()> {
        self.artifact_ref.validate()?;
        if self.source_event_id.trim().is_empty()
            || self.source_stream_sequence == 0
            || self.source_message_id.trim().is_empty()
            || self.artifact_sha256.len() != 71
            || !self.artifact_sha256.starts_with("sha256:")
            || self.call_id.trim().is_empty()
            || self.tool_name.trim().is_empty()
        {
            bail!("tool-output artifact source binding is malformed");
        }
        Ok(())
    }
}

impl ToolOutputPressureItemV1 {
    #[must_use]
    pub fn reclaimable_tokens(&self) -> u64 {
        if self.retention != ToolOutputRetentionClassV1::Ageable {
            return 0;
        }
        self.current_model_tokens
            .saturating_sub(TOOL_OUTPUT_AGED_RESULT_TARGET_TOKENS)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolOutputPressureSnapshotV1 {
    pub projection_schema_version: u16,
    pub policy_version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ProjectionCursor>,
    pub active_epoch_id: String,
    pub total_tool_tokens: u64,
    pub protected_tool_tokens: u64,
    pub reclaimable_tool_tokens: u64,
    pub ageable_count: u32,
    pub high_signal_count: u32,
    /// Aggregate tokens for already-aged results retired from the bounded working set.
    pub archived_aged_tool_tokens: u64,
    /// Number of already-aged results retired from the bounded working set.
    pub archived_aged_count: u64,
    /// Compact retrieval/GC bindings for retired aged results, keyed by opaque artifact id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub archived_artifact_bindings: BTreeMap<String, ToolOutputArchivedArtifactBindingV1>,
    pub items: Vec<ToolOutputPressureItemV1>,
}

impl ToolOutputPressureSnapshotV1 {
    /// Projects conservative artifact-GC roots from this already-incremental, body-free snapshot.
    ///
    /// This conversion performs no filesystem or session-log reads. Every descriptor still
    /// represented by the active result projection remains a root; narrower sets preserve why a
    /// descriptor is additionally protected by the current context, an unresolved call pair, or
    /// review-worthy high-signal facts.
    #[must_use]
    pub fn artifact_gc_roots(&self) -> ToolArtifactGcRootsV1 {
        let mut roots = ToolArtifactGcRootsV1::default();
        roots.active_result_refs.extend(
            self.archived_artifact_bindings
                .values()
                .map(|binding| binding.artifact_ref.clone()),
        );
        roots.context_epoch_refs.extend(
            self.archived_artifact_bindings
                .values()
                .map(|binding| binding.artifact_ref.clone()),
        );
        for item in &self.items {
            let Some(artifact_ref) = item.artifact_ref.clone() else {
                continue;
            };
            roots.active_result_refs.insert(artifact_ref.clone());
            roots.context_epoch_refs.insert(artifact_ref.clone());
            if !item.pair_closed {
                roots.unresolved_read_refs.insert(artifact_ref.clone());
            }
            if item.high_signal {
                roots.verification_review_pins.insert(artifact_ref);
            }
        }
        roots
    }

    /// Resolves an opaque ref to its active or compact archived source binding without JSONL I/O.
    #[must_use]
    pub fn artifact_source_binding(
        &self,
        artifact_ref: &ToolArtifactRefV1,
    ) -> Option<ToolOutputArchivedArtifactBindingV1> {
        if let Some(item) = self
            .items
            .iter()
            .find(|item| item.artifact_ref.as_ref() == Some(artifact_ref))
        {
            return artifact_binding_from_item(item, false);
        }
        self.archived_artifact_bindings
            .get(&artifact_ref.artifact_id)
            .filter(|binding| &binding.artifact_ref == artifact_ref)
            .cloned()
    }

    /// Materializes every active and archived body-free source binding for model-read authority.
    ///
    /// The result is bounded by the manifest-aligned session hard cap and rejects duplicate opaque
    /// refs instead of allowing call-site-specific last-write-wins behavior.
    pub fn artifact_source_bindings(&self) -> Result<Vec<ToolOutputArchivedArtifactBindingV1>> {
        let mut bindings = Vec::new();
        let mut seen = BTreeSet::new();
        for binding in self
            .items
            .iter()
            .filter_map(|item| artifact_binding_from_item(item, false))
            .chain(self.archived_artifact_bindings.values().cloned())
        {
            if bindings.len() == TOOL_OUTPUT_PRESSURE_HARD_MAX_RESULTS {
                bail!("tool-output artifact source bindings reached their hard cap");
            }
            binding.validate()?;
            if !seen.insert(binding.artifact_ref.clone()) {
                bail!("tool-output artifact source bindings repeat an opaque ref");
            }
            bindings.push(binding);
        }
        Ok(bindings)
    }
}

#[derive(Debug, Clone)]
pub struct ToolOutputPressureProjectionV1 {
    cursor: Option<ProjectionCursor>,
    active_epoch_id: String,
    turn_index: u64,
    items: BTreeMap<String, ToolOutputPressureItemV1>,
    ordered_ids: Vec<String>,
    current_turn_ids: Vec<String>,
    recent_ids: VecDeque<String>,
    recent_tokens: u64,
    open_tool_calls: BTreeSet<String>,
    result_ids_by_call: BTreeMap<String, String>,
    result_ids_by_message: BTreeMap<String, String>,
    mutation_calls_by_operation: BTreeMap<String, String>,
    pending_signals_by_call: BTreeMap<String, ToolOutputSignalEnrichmentV1>,
    archived_aged_tool_tokens: u64,
    archived_aged_count: u64,
    archived_artifact_bindings: BTreeMap<String, ToolOutputArchivedArtifactBindingV1>,
}

impl Default for ToolOutputPressureProjectionV1 {
    fn default() -> Self {
        Self {
            cursor: None,
            active_epoch_id: "context-epoch:root".to_owned(),
            turn_index: 0,
            items: BTreeMap::new(),
            ordered_ids: Vec::new(),
            current_turn_ids: Vec::new(),
            recent_ids: VecDeque::new(),
            recent_tokens: 0,
            open_tool_calls: BTreeSet::new(),
            result_ids_by_call: BTreeMap::new(),
            result_ids_by_message: BTreeMap::new(),
            mutation_calls_by_operation: BTreeMap::new(),
            pending_signals_by_call: BTreeMap::new(),
            archived_aged_tool_tokens: 0,
            archived_aged_count: 0,
            archived_artifact_bindings: BTreeMap::new(),
        }
    }
}

impl ToolOutputPressureProjectionV1 {
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let mut projection = Self::default();
        projection.apply_records(records)?;
        Ok(projection)
    }

    pub fn apply_records(&mut self, records: &[SessionStreamRecord]) -> Result<()> {
        for record in records {
            let expected_sequence = self
                .cursor
                .as_ref()
                .map_or(1, |cursor| cursor.last_applied_stream_sequence + 1);
            if record.stream_sequence() != expected_sequence {
                bail!(
                    "tool-output pressure projection cursor gap: expected {}, got {}",
                    expected_sequence,
                    record.stream_sequence()
                );
            }
            if let Some(entry) = session_entry_from_stored_event(record.stored_event())? {
                match entry {
                    SessionLogEntry::User(_) => self.begin_turn(),
                    SessionLogEntry::Assistant(message) => {
                        for call_id in message
                            .tool_calls
                            .into_iter()
                            .filter_map(|call| (!call.id.trim().is_empty()).then_some(call.id))
                        {
                            if self.open_tool_calls.len() >= TOOL_OUTPUT_OPEN_CALL_MAX {
                                bail!("tool-output pressure projection exceeded its open-call cap");
                            }
                            self.open_tool_calls.insert(call_id);
                        }
                    }
                    SessionLogEntry::ToolResultV2(result) => {
                        self.trim_aged_items_to_soft_limit(
                            TOOL_OUTPUT_PRESSURE_MAX_RESULTS.saturating_sub(1),
                        )?;
                        validate_pressure_result_capacity(self.items.len())?;
                        let pair_closed = self.open_tool_calls.remove(&result.call_id);
                        let item = pressure_item(
                            record.event_id(),
                            record.stream_sequence(),
                            self.turn_index,
                            pair_closed,
                            result,
                        );
                        let key = item.source_event_id.clone();
                        if self.items.insert(key.clone(), item).is_some() {
                            bail!("tool-output pressure projection repeated a result event id");
                        }
                        let item = self.items.get(&key).expect("inserted pressure item");
                        if self
                            .result_ids_by_call
                            .insert(item.call_id.clone(), key.clone())
                            .is_some()
                        {
                            bail!("tool-output pressure projection repeated a tool call id");
                        }
                        if self
                            .result_ids_by_message
                            .insert(item.message_id.clone(), key.clone())
                            .is_some()
                        {
                            bail!("tool-output pressure projection repeated a result message id");
                        }
                        self.ordered_ids.push(key.clone());
                        self.current_turn_ids.push(key.clone());
                        self.recent_ids.push_back(key.clone());
                        self.recent_tokens = self.recent_tokens.saturating_add(
                            self.items
                                .get(&key)
                                .expect("inserted pressure item")
                                .current_model_tokens,
                        );
                        self.apply_pending_signals(&key);
                        self.rebalance_recent_window();
                        self.refresh_retention(&key);
                    }
                    SessionLogEntry::Control(control) => {
                        self.apply_control_signal(record.event_id(), control)?;
                    }
                }
            }
            if record.stored_event().event_kind() == Some(DurableEventType::MutationPrepared) {
                let prepared: MutationPrepared = serde_json::from_value(
                    record.stored_event().payload.clone(),
                )
                .context("failed to decode mutation preparation for tool-output pressure")?;
                self.apply_mutation_prepared(prepared)?;
            } else if record.stored_event().event_kind()
                == Some(DurableEventType::MutationCommitted)
            {
                let committed: MutationCommitted =
                    serde_json::from_value(record.stored_event().payload.clone())
                        .context("failed to decode mutation commit for tool-output pressure")?;
                self.apply_mutation_committed(record.event_id(), committed)?;
            } else if record.stored_event().event_kind()
                == Some(DurableEventType::ToolOutputAgingActivated)
            {
                let activation: ToolOutputAgingActivatedV1 =
                    serde_json::from_value(record.stored_event().payload.clone())
                        .context("failed to decode tool-output aging activation")?;
                activation.validate_against_snapshot(&self.snapshot())?;
                self.apply_activation(&activation)?;
            } else if record.stored_event().event_kind()
                == Some(DurableEventType::ToolOutputProjectionShrinkRecorded)
            {
                let sidecar: super::ToolOutputProjectionShrinkRecorded =
                    serde_json::from_value(record.stored_event().payload.clone())
                        .context("failed to decode tool-output context epoch transition")?;
                sidecar.validate_shape()?;
                self.active_epoch_id = sidecar.epoch_transition.target_epoch_id;
            } else if record.stored_event().event_kind()
                == Some(DurableEventType::CompactionAppliedV2)
            {
                let applied: super::CompactionAppliedV2 =
                    serde_json::from_value(record.stored_event().payload.clone())
                        .context("failed to decode active tool-output compaction epoch")?;
                self.active_epoch_id = format!("context-epoch:{}", applied.compaction_id);
            }
            self.cursor =
                Some(record.projection_cursor(TOOL_OUTPUT_PRESSURE_PROJECTION_SCHEMA_VERSION));
        }
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> ToolOutputPressureSnapshotV1 {
        let items = self
            .ordered_ids
            .iter()
            .filter_map(|id| self.items.get(id).cloned())
            .collect::<Vec<_>>();
        ToolOutputPressureSnapshotV1 {
            projection_schema_version: TOOL_OUTPUT_PRESSURE_PROJECTION_SCHEMA_VERSION,
            policy_version: TOOL_OUTPUT_AGING_POLICY_VERSION,
            cursor: self.cursor.clone(),
            active_epoch_id: self.active_epoch_id.clone(),
            total_tool_tokens: items
                .iter()
                .map(|item| item.current_model_tokens)
                .sum::<u64>()
                .saturating_add(self.archived_aged_tool_tokens),
            protected_tool_tokens: items
                .iter()
                .filter(|item| {
                    matches!(
                        item.retention,
                        ToolOutputRetentionClassV1::CurrentTurn
                            | ToolOutputRetentionClassV1::UnpairedProtected
                            | ToolOutputRetentionClassV1::HighSignalProtected
                            | ToolOutputRetentionClassV1::RecentProtected
                    )
                })
                .map(|item| item.current_model_tokens)
                .sum(),
            reclaimable_tool_tokens: items
                .iter()
                .map(ToolOutputPressureItemV1::reclaimable_tokens)
                .sum(),
            ageable_count: items
                .iter()
                .filter(|item| item.retention == ToolOutputRetentionClassV1::Ageable)
                .count()
                .try_into()
                .unwrap_or(u32::MAX),
            high_signal_count: items
                .iter()
                .filter(|item| item.high_signal)
                .count()
                .try_into()
                .unwrap_or(u32::MAX),
            archived_aged_tool_tokens: self.archived_aged_tool_tokens,
            archived_aged_count: self.archived_aged_count,
            archived_artifact_bindings: self.archived_artifact_bindings.clone(),
            items,
        }
    }

    fn begin_turn(&mut self) {
        self.turn_index = self.turn_index.saturating_add(1);
        let recent = self.recent_ids.iter().cloned().collect::<BTreeSet<_>>();
        for id in self.current_turn_ids.drain(..) {
            if let Some(item) = self.items.get_mut(&id) {
                item.retention = if !item.pair_closed {
                    ToolOutputRetentionClassV1::UnpairedProtected
                } else if item.high_signal {
                    ToolOutputRetentionClassV1::HighSignalProtected
                } else if recent.contains(&id) {
                    ToolOutputRetentionClassV1::RecentProtected
                } else {
                    ToolOutputRetentionClassV1::Ageable
                };
            }
        }
    }

    fn rebalance_recent_window(&mut self) {
        while self.recent_ids.len() > 1 && self.recent_tokens > TOOL_OUTPUT_RECENT_PROTECTED_TOKENS
        {
            let Some(id) = self.recent_ids.pop_front() else {
                break;
            };
            if let Some(item) = self.items.get(&id) {
                self.recent_tokens = self.recent_tokens.saturating_sub(item.current_model_tokens);
            }
            self.refresh_retention(&id);
        }
    }

    fn refresh_retention(&mut self, id: &str) {
        let is_current = self.current_turn_ids.iter().any(|value| value == id);
        let is_recent = self.recent_ids.iter().any(|value| value == id);
        if let Some(item) = self.items.get_mut(id) {
            item.retention =
                if item.retention == ToolOutputRetentionClassV1::Aged && !item.high_signal {
                    ToolOutputRetentionClassV1::Aged
                } else if !item.pair_closed {
                    ToolOutputRetentionClassV1::UnpairedProtected
                } else if is_current {
                    ToolOutputRetentionClassV1::CurrentTurn
                } else if item.high_signal {
                    ToolOutputRetentionClassV1::HighSignalProtected
                } else if is_recent {
                    ToolOutputRetentionClassV1::RecentProtected
                } else {
                    ToolOutputRetentionClassV1::Ageable
                };
        }
    }

    fn apply_control_signal(&mut self, event_id: &str, control: ControlEntry) -> Result<()> {
        match control {
            ControlEntry::ToolApproval(approval) => {
                self.apply_signal_to_call(
                    &approval.call_id,
                    ToolOutputSignalEnrichmentV1 {
                        approval_receipt_refs: vec![event_id.to_owned()],
                        ..ToolOutputSignalEnrichmentV1::default()
                    },
                )?;
            }
            ControlEntry::ToolApprovalSessionGrant(grant) => {
                self.apply_signal_to_call(
                    &grant.source_call_id,
                    ToolOutputSignalEnrichmentV1 {
                        approval_receipt_refs: vec![event_id.to_owned()],
                        ..ToolOutputSignalEnrichmentV1::default()
                    },
                )?;
            }
            ControlEntry::ToolExecution(execution) => {
                let mut signal = ToolOutputSignalEnrichmentV1 {
                    changed_files: execution.changed_files,
                    marks_error: matches!(
                        execution.status,
                        ToolExecutionStatus::Failed
                            | ToolExecutionStatus::Cancelled
                            | ToolExecutionStatus::Interrupted
                    ) || execution.metadata.exit_code.is_some_and(|code| code != 0),
                    ..ToolOutputSignalEnrichmentV1::default()
                };
                if let Some(receipt) = execution.metadata.receipt {
                    signal
                        .mutation_receipt_refs
                        .extend(receipt.mutation_operation_ids);
                }
                self.apply_signal_to_call(&execution.call_id, signal)?;
            }
            ControlEntry::VerificationRecorded(verification) => {
                let receipt = verification.receipt.receipt;
                let signal = ToolOutputSignalEnrichmentV1 {
                    verification_receipt_refs: vec![receipt.receipt_id.clone()],
                    marks_error: verification.receipt.check_status == crate::ReceiptStatus::Failed,
                    ..ToolOutputSignalEnrichmentV1::default()
                };
                if let Some(call_id) = receipt.producer_tool_call {
                    self.apply_signal_to_call(&call_id, signal.clone())?;
                }
                self.apply_signal_to_source_event(&receipt.source_event_id, signal);
            }
            ControlEntry::ExternalProvenance(provenance) => {
                self.apply_signal_to_message(
                    &provenance.message_id,
                    ToolOutputSignalEnrichmentV1 {
                        external_provenance_refs: vec![event_id.to_owned()],
                        ..ToolOutputSignalEnrichmentV1::default()
                    },
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_mutation_prepared(&mut self, prepared: MutationPrepared) -> Result<()> {
        let Some(call_id) = prepared.tool_call_id else {
            return Ok(());
        };
        // This map is bounded by unresolved prepared mutations because commit consumes its entry.
        // The high ceiling guards malformed streams without coupling capacity to session age.
        if self.mutation_calls_by_operation.len() == TOOL_OUTPUT_MUTATION_LINK_MAX
            && !self
                .mutation_calls_by_operation
                .contains_key(&prepared.operation_id)
        {
            bail!("tool-output pressure projection exceeded its unresolved mutation-link cap");
        }
        if self
            .mutation_calls_by_operation
            .insert(prepared.operation_id, call_id)
            .is_some()
        {
            bail!("tool-output pressure projection repeated a mutation operation id");
        }
        Ok(())
    }

    fn apply_mutation_committed(
        &mut self,
        _event_id: &str,
        committed: MutationCommitted,
    ) -> Result<()> {
        let Some(call_id) = self
            .mutation_calls_by_operation
            .remove(&committed.operation_id)
        else {
            return Ok(());
        };
        let changed_files = match committed.committed_subject {
            MutationSubject::File { path, .. } | MutationSubject::Directory { path } => {
                vec![path.display().to_string()]
            }
            MutationSubject::Workspace { .. }
            | MutationSubject::External { .. }
            | MutationSubject::Unknown => Vec::new(),
        };
        self.apply_signal_to_call(
            &call_id,
            ToolOutputSignalEnrichmentV1 {
                mutation_receipt_refs: vec![committed.operation_id],
                changed_files,
                ..ToolOutputSignalEnrichmentV1::default()
            },
        )
    }

    fn apply_pending_signals(&mut self, item_id: &str) {
        let Some(item) = self.items.get(item_id) else {
            return;
        };
        let call_id = item.call_id.clone();
        let pending = self.pending_signals_by_call.remove(&call_id);
        if let Some(signal) = pending {
            self.apply_signal_to_item(item_id, signal);
        }
    }

    fn apply_signal_to_call(
        &mut self,
        call_id: &str,
        signal: ToolOutputSignalEnrichmentV1,
    ) -> Result<()> {
        if let Some(item_id) = self.result_ids_by_call.get(call_id).cloned() {
            self.apply_signal_to_item(&item_id, signal);
            return Ok(());
        }
        if !self.open_tool_calls.contains(call_id) {
            return Ok(());
        }
        self.pending_signals_by_call
            .entry(call_id.to_owned())
            .or_default()
            .merge(signal);
        Ok(())
    }

    fn apply_signal_to_message(
        &mut self,
        message_id: &str,
        signal: ToolOutputSignalEnrichmentV1,
    ) -> Result<()> {
        if let Some(item_id) = self.result_ids_by_message.get(message_id).cloned() {
            self.apply_signal_to_item(&item_id, signal);
        }
        Ok(())
    }

    fn apply_signal_to_source_event(
        &mut self,
        source_event_id: &str,
        signal: ToolOutputSignalEnrichmentV1,
    ) {
        if self.items.contains_key(source_event_id) {
            self.apply_signal_to_item(source_event_id, signal);
        }
    }

    fn apply_signal_to_item(&mut self, item_id: &str, signal: ToolOutputSignalEnrichmentV1) {
        if let Some(item) = self.items.get_mut(item_id) {
            signal.apply_to(&mut item.facts);
            item.high_signal = high_signal_facts(&item.facts);
        }
        self.refresh_retention(item_id);
    }

    fn apply_activation(&mut self, activation: &ToolOutputAgingActivatedV1) -> Result<()> {
        for replacement in &activation.replacements {
            let item = self
                .items
                .get_mut(&replacement.source_event_id)
                .context("tool-output aging activation source is missing")?;
            item.current_model_tokens = replacement.aged_model_view.token_upper_bound;
            item.retention = ToolOutputRetentionClassV1::Aged;
        }
        self.active_epoch_id.clone_from(&activation.target_epoch_id);
        self.trim_aged_items_to_soft_limit(TOOL_OUTPUT_PRESSURE_MAX_RESULTS)?;
        Ok(())
    }

    fn trim_aged_items_to_soft_limit(&mut self, target_len: usize) -> Result<()> {
        while self.items.len() > target_len {
            let Some(id) = self.ordered_ids.iter().find_map(|id| {
                self.items
                    .get(id)
                    .is_some_and(|item| {
                        item.retention == ToolOutputRetentionClassV1::Aged && !item.high_signal
                    })
                    .then(|| id.clone())
            }) else {
                // Unaged/protected entries describe the active context and cannot be dropped
                // without first publishing an append-only aging activation.
                break;
            };
            let Some(item) = self.items.get(&id) else {
                continue;
            };
            let archived_binding = artifact_binding_from_item(item, true);
            if let Some(binding) = archived_binding.as_ref()
                && self.archived_artifact_bindings.len()
                    == TOOL_OUTPUT_ARCHIVED_ARTIFACT_BINDING_MAX
                && !self
                    .archived_artifact_bindings
                    .contains_key(&binding.artifact_ref.artifact_id)
            {
                bail!("tool-output archived artifact binding manifest reached its hard cap");
            }
            let Some(item) = self.items.remove(&id) else {
                continue;
            };
            self.ordered_ids.retain(|value| value != &id);
            self.current_turn_ids.retain(|value| value != &id);
            self.recent_ids.retain(|value| value != &id);
            self.result_ids_by_call.remove(&item.call_id);
            self.result_ids_by_message.remove(&item.message_id);
            self.archived_aged_tool_tokens = self
                .archived_aged_tool_tokens
                .saturating_add(item.current_model_tokens);
            self.archived_aged_count = self.archived_aged_count.saturating_add(1);
            if let Some(binding) = archived_binding {
                self.archived_artifact_bindings
                    .insert(binding.artifact_ref.artifact_id.clone(), binding);
            }
        }
        Ok(())
    }
}

fn validate_pressure_result_capacity(current_len: usize) -> Result<()> {
    // Normal pre-turn FitRequired aging should rotate old ordinary results before this tier grows.
    // A soft-limit overflow is still safer than dropping active context; only the manifest-aligned
    // hard boundary rejects a stream.
    if current_len >= TOOL_OUTPUT_PRESSURE_HARD_MAX_RESULTS {
        bail!("tool-output pressure projection reached its manifest-aligned hard result cap");
    }
    Ok(())
}

fn artifact_binding_from_item(
    item: &ToolOutputPressureItemV1,
    archived: bool,
) -> Option<ToolOutputArchivedArtifactBindingV1> {
    Some(ToolOutputArchivedArtifactBindingV1 {
        artifact_ref: item.artifact_ref.clone()?,
        source_event_id: item.source_event_id.clone(),
        source_stream_sequence: item.source_stream_sequence,
        source_message_id: item.message_id.clone(),
        artifact_sha256: item.artifact_sha256.clone()?,
        persisted_bytes: item.persisted_bytes,
        call_id: item.call_id.clone(),
        tool_name: item.tool_name.clone(),
        artifact_availability: item.artifact_availability,
        archived,
    })
}

fn pressure_item(
    event_id: &str,
    stream_sequence: u64,
    turn_index: u64,
    pair_closed: bool,
    result: super::ToolResultRecordedV2,
) -> ToolOutputPressureItemV1 {
    let preview_excerpt = bounded_pressure_excerpt(&result.initial_model_view.preview);
    let (artifact_ref, artifact_sha256, availability, complete, observed_bytes, persisted_bytes) =
        match result.artifact {
            ToolArtifactBindingV1::Published { descriptor } => {
                let availability = if descriptor.retrieval_available() {
                    ToolArtifactAvailability::Available
                } else {
                    ToolArtifactAvailability::PolicyRevoked
                };
                (
                    Some(descriptor.artifact_ref),
                    Some(descriptor.content_sha256),
                    availability,
                    matches!(descriptor.completeness, ToolArtifactCompleteness::Complete),
                    descriptor.observed_bytes,
                    descriptor.persisted_bytes,
                )
            }
            ToolArtifactBindingV1::Unavailable { unavailable } => (
                None,
                None,
                unavailable.availability,
                false,
                unavailable.observed_bytes,
                0,
            ),
        };
    let tokens = result.initial_model_view.token_upper_bound.max(1);
    let facts = result.facts;
    ToolOutputPressureItemV1 {
        source_event_id: event_id.to_owned(),
        source_stream_sequence: stream_sequence,
        message_id: result.message_id,
        call_id: result.call_id,
        tool_name: result.tool_name,
        artifact_ref,
        artifact_sha256,
        artifact_availability: availability,
        complete,
        observed_bytes,
        persisted_bytes,
        initial_model_tokens: tokens,
        current_model_tokens: tokens,
        preview_excerpt,
        high_signal: high_signal_facts(&facts),
        pair_closed,
        retention: if pair_closed {
            ToolOutputRetentionClassV1::CurrentTurn
        } else {
            ToolOutputRetentionClassV1::UnpairedProtected
        },
        turn_index,
        initial_model_view_sha256: result.initial_model_view_sha256,
        facts,
    }
}

fn bounded_pressure_excerpt(value: &str) -> String {
    const MAX_BYTES: usize = 1_024;
    if value.len() <= MAX_BYTES {
        return value.to_owned();
    }
    let mut boundary = MAX_BYTES;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}…", &value[..boundary])
}

fn high_signal_facts(facts: &ToolResultFactsV1) -> bool {
    facts.status == "error"
        || facts.exit_code.is_some_and(|code| code != 0)
        || !facts.changed_files.is_empty()
        || !facts.approval_receipt_refs.is_empty()
        || !facts.mutation_receipt_refs.is_empty()
        || !facts.verification_receipt_refs.is_empty()
        || !facts.external_provenance_refs.is_empty()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputAgingReasonV1 {
    FitRequired,
    CostOnly,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolOutputAgingBatchV1 {
    pub policy_version: u16,
    pub source_cursor: ProjectionCursor,
    pub source_epoch_id: String,
    pub reason: ToolOutputAgingReasonV1,
    pub source_event_ids: Vec<String>,
    pub tokens_before: u64,
    pub tokens_after_upper_bound: u64,
    pub reclaimable_tokens: u64,
}

impl ToolOutputAgingBatchV1 {
    pub fn select(
        snapshot: &ToolOutputPressureSnapshotV1,
        reason: ToolOutputAgingReasonV1,
    ) -> Result<Option<Self>> {
        let cursor = snapshot
            .cursor
            .clone()
            .context("tool-output pressure snapshot has no source cursor")?;
        let mut ids = Vec::new();
        let mut reclaimable = 0_u64;
        let mut selected_tokens = 0_u64;
        for item in &snapshot.items {
            if item.retention != ToolOutputRetentionClassV1::Ageable {
                continue;
            }
            let reclaim = item.reclaimable_tokens();
            if reclaim == 0 {
                continue;
            }
            ids.push(item.source_event_id.clone());
            reclaimable = reclaimable.saturating_add(reclaim);
            selected_tokens = selected_tokens.saturating_add(item.current_model_tokens);
            if ids.len() == TOOL_OUTPUT_AGING_MAX_RESULTS
                || (reason != ToolOutputAgingReasonV1::FitRequired
                    && reclaimable >= TOOL_OUTPUT_MIN_BATCH_RECLAIM_TOKENS)
            {
                break;
            }
        }
        if ids.is_empty()
            || (reason == ToolOutputAgingReasonV1::CostOnly
                && reclaimable < TOOL_OUTPUT_MIN_BATCH_RECLAIM_TOKENS)
        {
            return Ok(None);
        }
        let tokens_after_upper_bound = snapshot
            .total_tool_tokens
            .saturating_sub(selected_tokens)
            .saturating_add(
                (ids.len() as u64).saturating_mul(TOOL_OUTPUT_AGED_RESULT_TARGET_TOKENS),
            );
        Ok(Some(Self {
            policy_version: TOOL_OUTPUT_AGING_POLICY_VERSION,
            source_cursor: cursor,
            source_epoch_id: snapshot.active_epoch_id.clone(),
            reason,
            source_event_ids: ids,
            tokens_before: snapshot.total_tool_tokens,
            tokens_after_upper_bound,
            reclaimable_tokens: reclaimable,
        }))
    }
}

/// One deterministic provider-facing replacement activated for a historical V2 tool result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolOutputAgedViewV1 {
    pub source_event_id: EventId,
    pub source_stream_sequence: u64,
    pub source_message_id: String,
    pub call_id: String,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<ToolArtifactRefV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_sha256: Option<String>,
    pub artifact_availability: ToolArtifactAvailability,
    pub source_initial_model_view_sha256: String,
    pub source_model_tokens: u64,
    pub aged_model_view: ToolModelViewV1,
}

impl ToolOutputAgedViewV1 {
    fn validate_shape(&self) -> Result<()> {
        if self.source_event_id.trim().is_empty()
            || self.source_stream_sequence == 0
            || self.source_message_id.trim().is_empty()
            || self.call_id.trim().is_empty()
            || self.tool_name.trim().is_empty()
            || self.source_initial_model_view_sha256.len() != 71
            || !self.source_initial_model_view_sha256.starts_with("sha256:")
            || self.source_model_tokens == 0
            || self.aged_model_view.preview_kind != ToolPreviewKind::Aged
            || self.aged_model_view.preview.len() > TOOL_OUTPUT_AGED_RESULT_MAX_BYTES
            || self.aged_model_view.token_upper_bound > TOOL_OUTPUT_AGED_RESULT_TARGET_TOKENS
        {
            bail!("tool-output aged view is malformed");
        }
        if let Some(reference) = &self.artifact_ref {
            reference.validate()?;
        }
        if let Some(hash) = &self.artifact_sha256
            && (hash.len() != 71 || !hash.starts_with("sha256:"))
        {
            bail!("tool-output aged view artifact hash is malformed");
        }
        if self.artifact_availability == ToolArtifactAvailability::Available
            && (self.artifact_ref.is_none() || self.artifact_sha256.is_none())
        {
            bail!("available tool-output aged view has no artifact identity");
        }
        self.aged_model_view.validate()
    }
}

/// Append-only activation of one cache-rotating deterministic tool-output aging batch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolOutputAgingActivatedV1 {
    pub schema_version: u16,
    pub policy_version: u16,
    pub source_cursor: ProjectionCursor,
    pub source_epoch_id: String,
    pub target_epoch_id: String,
    pub source_layout_hash: String,
    pub reason: ToolOutputAgingReasonV1,
    pub tokens_before: u64,
    pub tokens_after_upper_bound: u64,
    pub reclaimable_tokens: u64,
    pub replacements: Vec<ToolOutputAgedViewV1>,
}

impl ToolOutputAgingActivatedV1 {
    /// Materializes a body-free deterministic activation from one bounded active projection.
    ///
    /// No artifact body or session JSONL replay is needed at this stage.
    pub fn prepare(
        snapshot: &ToolOutputPressureSnapshotV1,
        batch: &ToolOutputAgingBatchV1,
    ) -> Result<Self> {
        let snapshot_cursor = snapshot
            .cursor
            .as_ref()
            .context("tool-output pressure snapshot has no source cursor")?;
        if &batch.source_cursor != snapshot_cursor
            || batch.source_epoch_id != snapshot.active_epoch_id
            || batch.policy_version != TOOL_OUTPUT_AGING_POLICY_VERSION
            || batch.source_event_ids.is_empty()
            || batch.source_event_ids.len() > TOOL_OUTPUT_AGING_MAX_RESULTS
        {
            bail!("tool-output aging batch is stale or malformed");
        }
        let by_id = snapshot
            .items
            .iter()
            .map(|item| (item.source_event_id.as_str(), item))
            .collect::<BTreeMap<_, _>>();
        let mut replacements = Vec::with_capacity(batch.source_event_ids.len());
        for source_event_id in &batch.source_event_ids {
            let item = by_id
                .get(source_event_id.as_str())
                .context("tool-output aging source is absent from the active projection")?;
            if item.retention != ToolOutputRetentionClassV1::Ageable || !item.pair_closed {
                bail!("tool-output aging source is protected");
            }
            replacements.push(aged_view_from_pressure_item(item)?);
        }
        let source_layout_hash = source_layout_hash(
            snapshot_cursor,
            &batch.source_epoch_id,
            batch.reason,
            &replacements,
        )?;
        let target_epoch_id = format!(
            "context-epoch:tool-aging-{}",
            stable_event_uuid(
                "sigil-tool-output-aging-epoch-v1",
                &format!(
                    "{}:{}:{}",
                    snapshot_cursor.session_id,
                    snapshot_cursor.last_applied_event_id,
                    source_layout_hash
                )
            )
        );
        let aged_tokens = replacements
            .iter()
            .map(|replacement| replacement.aged_model_view.token_upper_bound)
            .sum::<u64>();
        let selected_tokens = replacements
            .iter()
            .map(|replacement| replacement.source_model_tokens)
            .sum::<u64>();
        let activation = Self {
            schema_version: TOOL_OUTPUT_AGING_ACTIVATION_SCHEMA_VERSION,
            policy_version: TOOL_OUTPUT_AGING_POLICY_VERSION,
            source_cursor: snapshot_cursor.clone(),
            source_epoch_id: batch.source_epoch_id.clone(),
            target_epoch_id,
            source_layout_hash,
            reason: batch.reason,
            tokens_before: snapshot.total_tool_tokens,
            tokens_after_upper_bound: snapshot
                .total_tool_tokens
                .saturating_sub(selected_tokens)
                .saturating_add(aged_tokens),
            reclaimable_tokens: selected_tokens.saturating_sub(aged_tokens),
            replacements,
        };
        activation.validate_against_snapshot(snapshot)?;
        Ok(activation)
    }

    pub fn validate_shape(&self) -> Result<()> {
        if self.schema_version != TOOL_OUTPUT_AGING_ACTIVATION_SCHEMA_VERSION
            || self.policy_version != TOOL_OUTPUT_AGING_POLICY_VERSION
            || self.source_cursor.projection_schema_version
                != TOOL_OUTPUT_PRESSURE_PROJECTION_SCHEMA_VERSION
            || self.source_cursor.session_id.trim().is_empty()
            || self.source_cursor.last_applied_stream_sequence == 0
            || self.source_cursor.last_applied_event_id.trim().is_empty()
            || self
                .source_cursor
                .last_applied_record_checksum
                .trim()
                .is_empty()
            || self.source_epoch_id.trim().is_empty()
            || self.target_epoch_id.trim().is_empty()
            || self.source_epoch_id == self.target_epoch_id
            || self.source_layout_hash.len() != 71
            || !self.source_layout_hash.starts_with("sha256:")
            || self.replacements.is_empty()
            || self.replacements.len() > TOOL_OUTPUT_AGING_MAX_RESULTS
        {
            bail!("tool-output aging activation is malformed");
        }
        let mut source_ids = BTreeSet::new();
        for replacement in &self.replacements {
            replacement.validate_shape()?;
            if !source_ids.insert(replacement.source_event_id.as_str()) {
                bail!("tool-output aging activation repeats a source event");
            }
        }
        if source_layout_hash(
            &self.source_cursor,
            &self.source_epoch_id,
            self.reason,
            &self.replacements,
        )? != self.source_layout_hash
        {
            bail!("tool-output aging activation layout hash does not match");
        }
        let selected_tokens = self
            .replacements
            .iter()
            .map(|replacement| replacement.source_model_tokens)
            .sum::<u64>();
        let aged_tokens = self
            .replacements
            .iter()
            .map(|replacement| replacement.aged_model_view.token_upper_bound)
            .sum::<u64>();
        if self.tokens_after_upper_bound
            != self
                .tokens_before
                .saturating_sub(selected_tokens)
                .saturating_add(aged_tokens)
            || self.reclaimable_tokens != selected_tokens.saturating_sub(aged_tokens)
        {
            bail!("tool-output aging activation token proof does not match");
        }
        Ok(())
    }

    pub fn validate_against_snapshot(&self, snapshot: &ToolOutputPressureSnapshotV1) -> Result<()> {
        self.validate_shape()?;
        if snapshot.cursor.as_ref() != Some(&self.source_cursor)
            || snapshot.active_epoch_id != self.source_epoch_id
            || snapshot.total_tool_tokens != self.tokens_before
        {
            bail!("tool-output aging activation source frontier or epoch is stale");
        }
        let by_id = snapshot
            .items
            .iter()
            .map(|item| (item.source_event_id.as_str(), item))
            .collect::<BTreeMap<_, _>>();
        for replacement in &self.replacements {
            let item = by_id
                .get(replacement.source_event_id.as_str())
                .context("tool-output aging activation source is missing")?;
            if item.retention != ToolOutputRetentionClassV1::Ageable
                || !item.pair_closed
                || item.source_stream_sequence != replacement.source_stream_sequence
                || item.message_id != replacement.source_message_id
                || item.call_id != replacement.call_id
                || item.tool_name != replacement.tool_name
                || item.artifact_ref != replacement.artifact_ref
                || item.artifact_sha256 != replacement.artifact_sha256
                || item.artifact_availability != replacement.artifact_availability
                || item.initial_model_view_sha256 != replacement.source_initial_model_view_sha256
                || item.current_model_tokens != replacement.source_model_tokens
            {
                bail!("tool-output aging activation source facts drifted");
            }
        }
        if self.reason == ToolOutputAgingReasonV1::CostOnly
            && self.reclaimable_tokens < TOOL_OUTPUT_MIN_BATCH_RECLAIM_TOKENS
        {
            bail!("cost-only tool-output aging activation has insufficient benefit");
        }
        Ok(())
    }

    #[must_use]
    pub fn event_id(&self) -> String {
        stable_event_uuid(
            "sigil-tool-output-aging-activation-v1",
            &format!("{}:{}", self.source_cursor.session_id, self.target_epoch_id),
        )
    }
}

fn aged_view_from_pressure_item(item: &ToolOutputPressureItemV1) -> Result<ToolOutputAgedViewV1> {
    let preview = bounded_utf8(
        &serde_json::to_string(&json!({
            "tool_name": item.tool_name,
            "call_id": item.call_id,
            "status": item.facts.status,
            "exit_code": item.facts.exit_code,
            "duration_ms": item.facts.duration_ms,
            "changed_files": item.facts.changed_files,
            "error": item.facts.error,
            "mutation_receipt_refs": item.facts.mutation_receipt_refs,
            "verification_receipt_refs": item.facts.verification_receipt_refs,
            "external_provenance_refs": item.facts.external_provenance_refs,
            "artifact_availability": item.artifact_availability,
            "artifact_complete": item.complete,
        }))
        .context("failed to encode deterministic aged tool-output facts")?,
        TOOL_OUTPUT_AGED_RESULT_MAX_BYTES,
    );
    let retrieval_available = item.artifact_availability == ToolArtifactAvailability::Available;
    let aged_model_view = ToolModelViewV1 {
        token_upper_bound: ((preview.len() as u64).saturating_add(3) / 4)
            .clamp(1, TOOL_OUTPUT_AGED_RESULT_TARGET_TOKENS),
        preview,
        preview_kind: ToolPreviewKind::Aged,
        artifact_ref: retrieval_available
            .then(|| item.artifact_ref.clone())
            .flatten(),
        retrieval_hint: retrieval_available.then_some(
            "Use read_tool_artifact with a bounded typed selector only when exact historical output is required."
                .to_owned(),
        ),
        projection_version: super::TOOL_MODEL_VIEW_SCHEMA_VERSION,
    };
    aged_model_view.validate()?;
    Ok(ToolOutputAgedViewV1 {
        source_event_id: item.source_event_id.clone(),
        source_stream_sequence: item.source_stream_sequence,
        source_message_id: item.message_id.clone(),
        call_id: item.call_id.clone(),
        tool_name: item.tool_name.clone(),
        artifact_ref: item.artifact_ref.clone(),
        artifact_sha256: item.artifact_sha256.clone(),
        artifact_availability: item.artifact_availability,
        source_initial_model_view_sha256: item.initial_model_view_sha256.clone(),
        source_model_tokens: item.current_model_tokens,
        aged_model_view,
    })
}

fn bounded_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn source_layout_hash(
    cursor: &ProjectionCursor,
    source_epoch_id: &str,
    reason: ToolOutputAgingReasonV1,
    replacements: &[ToolOutputAgedViewV1],
) -> Result<String> {
    let proof = json!({
        "policy_version": TOOL_OUTPUT_AGING_POLICY_VERSION,
        "source_cursor": cursor,
        "source_epoch_id": source_epoch_id,
        "reason": reason,
        "sources": replacements.iter().map(|replacement| json!({
            "source_event_id": replacement.source_event_id,
            "source_stream_sequence": replacement.source_stream_sequence,
            "artifact_ref": replacement.artifact_ref,
            "artifact_sha256": replacement.artifact_sha256,
            "artifact_availability": replacement.artifact_availability,
            "source_initial_model_view_sha256": replacement.source_initial_model_view_sha256,
            "source_model_tokens": replacement.source_model_tokens,
            "aged_model_view": replacement.aged_model_view,
        })).collect::<Vec<_>>(),
    });
    serde_json::to_vec(&proof)
        .map(stable_event_hash)
        .context("failed to encode tool-output aging source layout")
}

/// Replayable resolver for all V2 tool-output model-view replacements in the active epoch chain.
#[derive(Debug, Clone, Default)]
pub struct ToolOutputAgingProjectionV1 {
    active_epoch_id: String,
    replacements: BTreeMap<EventId, ToolOutputAgedViewV1>,
}

impl ToolOutputAgingProjectionV1 {
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let mut pressure = ToolOutputPressureProjectionV1::default();
        let mut projection = Self {
            active_epoch_id: "context-epoch:root".to_owned(),
            replacements: BTreeMap::new(),
        };
        for record in records {
            if record.stored_event().event_kind()
                == Some(DurableEventType::ToolOutputAgingActivated)
            {
                let activation: ToolOutputAgingActivatedV1 =
                    serde_json::from_value(record.stored_event().payload.clone())
                        .context("failed to decode tool-output aging activation")?;
                activation.validate_against_snapshot(&pressure.snapshot())?;
                for replacement in &activation.replacements {
                    projection
                        .replacements
                        .insert(replacement.source_event_id.clone(), replacement.clone());
                }
                projection
                    .active_epoch_id
                    .clone_from(&activation.target_epoch_id);
            }
            pressure.apply_records(std::slice::from_ref(record))?;
            projection.active_epoch_id = pressure.active_epoch_id.clone();
        }
        Ok(projection)
    }

    #[must_use]
    pub fn active_epoch_id(&self) -> &str {
        &self.active_epoch_id
    }

    #[must_use]
    pub fn replacements(&self) -> &BTreeMap<EventId, ToolOutputAgedViewV1> {
        &self.replacements
    }
}

impl JsonlSessionStore {
    /// Compares one prepared activation against the current in-memory frontier and publishes it.
    ///
    /// The compare-and-append path never reloads the JSONL stream.
    pub fn append_tool_output_aging_activation(
        &self,
        expected_frontier: &ActiveProjectionFrontier,
        activation: ToolOutputAgingActivatedV1,
    ) -> Result<Option<StoredEvent>> {
        activation.validate_shape()?;
        let event_id = activation.event_id();
        let payload = serde_json::to_value(&activation)
            .context("failed to encode tool-output aging activation")?;
        self.append_event_if_active_with_identity(
            DurableEventType::ToolOutputAgingActivated,
            payload,
            event_id.clone(),
            Some(event_id),
            None,
            expected_frontier,
            move |active| {
                activation
                    .validate_against_snapshot(&active.tool_output_pressure_snapshot())
                    .map(|()| true)
            },
        )
    }
}

#[cfg(test)]
#[path = "tests/tool_output_pressure_projection_tests.rs"]
mod tests;
