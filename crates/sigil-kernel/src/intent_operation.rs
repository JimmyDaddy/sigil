use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::intent_layer::{ExactIntentPatchFileV1, decode_exact_intent_patch};
use crate::{
    BoundedIntentArtifactSubjectV1, DurableEventType, EventClass, FileType,
    INTENT_CANONICAL_DIGEST_PREFIX, INTENT_CONTRACT_SCHEMA_VERSION, IntentArtifactAvailability,
    IntentArtifactId, IntentArtifactOwnership, IntentConflictV1, IntentContentDigest, IntentDigest,
    IntentEventV1, IntentLayerProjectionV1, IntentLayerStateV1, IntentOperationErrorCode,
    IntentOperationFileAction, IntentOperationFileSummaryV1, IntentOperationId,
    IntentOperationKind, IntentOperationPreviewV1, IntentOperationResolution,
    IntentStackProjectionV1, IntentVerificationImpact, IntentVerificationImpactV1,
    IntentVersionRef, JsonlSessionStore, MutationBatchFinished, MutationBatchStarted,
    MutationBatchStatus, MutationCommitted, MutationCoordinator, MutationEventRecorder,
    MutationObservedState, MutationPrepared, MutationReconciled, MutationResolution,
    MutationSubject, PreparedFileMutation, Session, SessionStreamRecord, TypedDomainEvent,
    TypedStoredEventDecode, decode_typed_stored_event, file_content_hash, stable_event_uuid,
    stable_workspace_id,
};

/// Projection schema for exact R51.4 operation recovery.
pub const INTENT_OPERATION_PROJECTION_SCHEMA_VERSION: u16 = 1;

/// Runtime-only approval authority for an exact intent operation.
///
/// It intentionally has no serde implementation: renderer, provider output and restored JSON
/// cannot manufacture approval authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentOperationAuthorityV1 {
    permission_policy_digest: IntentDigest,
    approval_authority_id: String,
    expires_at_ms: Option<u64>,
}

impl IntentOperationAuthorityV1 {
    /// Creates host-owned approval authority bound to one permission policy.
    pub fn new(
        permission_policy_digest: IntentDigest,
        approval_authority_id: impl Into<String>,
        expires_at_ms: Option<u64>,
    ) -> Result<Self> {
        let approval_authority_id = approval_authority_id.into();
        validate_runtime_identity("intent approval authority", &approval_authority_id)?;
        if expires_at_ms == Some(0) {
            bail!("intent approval authority expiry must be non-zero");
        }
        Ok(Self {
            permission_policy_digest,
            approval_authority_id,
            expires_at_ms,
        })
    }

    fn is_current(&self, now_ms: u64) -> bool {
        self.expires_at_ms.is_none_or(|expiry| now_ms <= expiry)
    }
}

/// Renderer-to-runtime request. It carries no path, bytes, hash, dependency closure or approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentDropRequestV1 {
    pub operation_id: IntentOperationId,
    pub stack_version: crate::IntentStackVersion,
    pub preview_digest: IntentDigest,
}

/// Rebuildable operation state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentOperationStateV1 {
    Requested,
    Prepared,
    Applying,
    Committed,
    Rejected,
    Cancelled,
    Conflicted,
    PartiallyApplied,
    Interrupted,
}

/// Bounded operation row for inspect and recovery surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentOperationSummaryV1 {
    pub operation_id: IntentOperationId,
    pub operation_kind: IntentOperationKind,
    pub target_intents: Vec<IntentVersionRef>,
    pub state: IntentOperationStateV1,
    pub mutation_batch_id: Option<String>,
    pub result_snapshot_id: Option<String>,
    pub error_code: Option<IntentOperationErrorCode>,
}

/// Exact execution result. Conflicts and explicit rejection are durable successful protocol
/// outcomes rather than unstructured errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentOperationExecutionV1 {
    pub preview: IntentOperationPreviewV1,
    pub resolution: IntentOperationResolution,
    pub mutation_batch_id: Option<String>,
    pub committed_operation_ids: Vec<String>,
    pub result_snapshot_id: Option<String>,
    pub error_code: Option<IntentOperationErrorCode>,
}

/// Append-only R51.4 operation projection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntentOperationProjectionV1 {
    pub operations: BTreeMap<IntentOperationId, IntentOperationSummaryV1>,
    pub conflicts: Vec<IntentConflictV1>,
    dropped_intents: BTreeSet<IntentVersionRef>,
}

impl IntentOperationProjectionV1 {
    /// Replays exact operation and RFC-0002 batch evidence.
    pub fn from_records(
        records: &[SessionStreamRecord],
        admission: &IntentStackProjectionV1,
        layers: &IntentLayerProjectionV1,
    ) -> Result<Self> {
        let replay = OperationReplay::from_records(records, admission, layers)?;
        Ok(replay.public_projection())
    }

    #[must_use]
    pub fn operation(&self, operation_id: &IntentOperationId) -> Option<&IntentOperationSummaryV1> {
        self.operations.get(operation_id)
    }

    #[must_use]
    pub fn is_dropped(&self, intent_ref: &IntentVersionRef) -> bool {
        self.dropped_intents.contains(intent_ref)
    }

    #[must_use]
    pub fn dropped_intents(&self) -> &BTreeSet<IntentVersionRef> {
        &self.dropped_intents
    }

    #[must_use]
    pub fn has_active_operation_for(&self, intent_ref: &IntentVersionRef) -> bool {
        self.operations.values().any(|operation| {
            operation.target_intents.contains(intent_ref)
                && matches!(
                    operation.state,
                    IntentOperationStateV1::Requested
                        | IntentOperationStateV1::Prepared
                        | IntentOperationStateV1::Applying
                )
        })
    }
}

impl Session {
    /// Rebuilds exact operation state from the durable stream.
    pub fn intent_operation_projection(&self) -> Result<IntentOperationProjectionV1> {
        let store = self
            .durable_store()
            .context("Intent operation projection requires a durable session")?;
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let admission = IntentStackProjectionV1::from_records(&records)?;
        let lineage = crate::IntentLineageProjectionV1::from_records(&records, &admission)?;
        let mut layers = IntentLayerProjectionV1::from_records(&records, &admission, &lineage)?;
        if let Some(recorder) = self.mutation_event_recorder() {
            layers.refresh_artifact_availability(&recorder)?;
        }
        IntentOperationProjectionV1::from_records(&records, &admission, &layers)
    }
}

#[derive(Debug, Clone)]
struct PreparedOperation {
    preview_digest: IntentDigest,
    permission_policy_digest: IntentDigest,
    approval_authority_id: String,
    mutation_batch_id: String,
}

#[derive(Debug, Clone)]
struct ResolvedOperation {
    resolution: IntentOperationResolution,
    mutation_batch_id: Option<String>,
    result_snapshot_id: Option<String>,
    error_code: Option<IntentOperationErrorCode>,
}

#[derive(Debug, Clone)]
struct OperationRecord {
    preview: IntentOperationPreviewV1,
    requested_sequence: u64,
    prepared: Option<PreparedOperation>,
    prepared_sequence: Option<u64>,
    resolved: Option<ResolvedOperation>,
    resolved_sequence: Option<u64>,
}

#[derive(Debug, Clone, Default)]
struct BatchEvidence {
    started: BTreeMap<String, (MutationBatchStarted, u64)>,
    finished: BTreeMap<String, (MutationBatchFinished, u64)>,
    prepared: BTreeMap<String, Vec<(MutationPrepared, u64)>>,
    committed: BTreeMap<String, Vec<(MutationCommitted, u64)>>,
    reconciled: BTreeMap<String, Vec<(MutationReconciled, u64)>>,
}

impl BatchEvidence {
    fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let mut facts = Self::default();
        for record in records {
            let event = record.stored_event();
            match event.event_kind() {
                Some(DurableEventType::MutationBatchStarted) => {
                    let value: MutationBatchStarted = serde_json::from_value(event.payload.clone())
                        .context("failed to decode Intent operation batch start")?;
                    if facts
                        .started
                        .insert(value.batch_id.clone(), (value, event.stream_sequence))
                        .is_some()
                    {
                        bail!("Intent operation mutation batch started more than once");
                    }
                }
                Some(DurableEventType::MutationBatchFinished) => {
                    let value: MutationBatchFinished =
                        serde_json::from_value(event.payload.clone())
                            .context("failed to decode Intent operation batch terminal")?;
                    if facts
                        .finished
                        .insert(value.batch_id.clone(), (value, event.stream_sequence))
                        .is_some()
                    {
                        bail!("Intent operation mutation batch finished more than once");
                    }
                }
                Some(DurableEventType::MutationPrepared) => {
                    let value: MutationPrepared = serde_json::from_value(event.payload.clone())
                        .context("failed to decode Intent operation mutation prepare")?;
                    if let Some(batch_id) = value.batch_id.clone() {
                        facts
                            .prepared
                            .entry(batch_id)
                            .or_default()
                            .push((value, event.stream_sequence));
                    }
                }
                Some(DurableEventType::MutationCommitted) => {
                    let value: MutationCommitted = serde_json::from_value(event.payload.clone())
                        .context("failed to decode Intent operation mutation commit")?;
                    if let Some(batch_id) = value.batch_id.clone() {
                        facts
                            .committed
                            .entry(batch_id)
                            .or_default()
                            .push((value, event.stream_sequence));
                    }
                }
                Some(DurableEventType::MutationReconciled) => {
                    let value: MutationReconciled =
                        serde_json::from_value(event.payload.clone())
                            .context("failed to decode Intent operation mutation reconciliation")?;
                    if let Some(batch_id) = value.batch_id.clone() {
                        facts
                            .reconciled
                            .entry(batch_id)
                            .or_default()
                            .push((value, event.stream_sequence));
                    }
                }
                _ => {}
            }
        }
        Ok(facts)
    }

    fn has_file_evidence(&self, batch_id: &str) -> bool {
        self.prepared
            .get(batch_id)
            .is_some_and(|entries| !entries.is_empty())
            || self
                .committed
                .get(batch_id)
                .is_some_and(|entries| !entries.is_empty())
            || self
                .reconciled
                .get(batch_id)
                .is_some_and(|entries| !entries.is_empty())
    }

    fn outcome(
        &self,
        operation: &OperationRecord,
        prepared: &PreparedOperation,
    ) -> Result<BatchOutcome> {
        let batch_id = prepared.mutation_batch_id.as_str();
        let expected_paths = operation
            .preview
            .file_effects
            .iter()
            .map(|file| file.normalized_relative_path.clone())
            .collect::<BTreeSet<_>>();
        let Some((started, started_sequence)) = self.started.get(batch_id) else {
            return Ok(BatchOutcome::NoBatch);
        };
        if operation
            .prepared_sequence
            .is_none_or(|sequence| *started_sequence <= sequence)
        {
            bail!("Intent operation mutation batch started before operation prepare");
        }
        if started.operation_id != operation.preview.operation_id.as_str()
            || started.prepared_digest.as_deref() != Some(prepared.preview_digest.as_str())
            || started.approval_identity.as_deref() != Some(prepared.approval_authority_id.as_str())
            || started.policy_fingerprint.as_deref()
                != Some(prepared.permission_policy_digest.as_str())
            || started
                .expected_subjects
                .iter()
                .filter_map(mutation_file_path)
                .collect::<BTreeSet<_>>()
                != expected_paths
            || started.expected_subjects.len() != expected_paths.len()
        {
            bail!("Intent operation batch start does not match its prepared authority");
        }
        let prepared_files = self.prepared.get(batch_id).cloned().unwrap_or_default();
        let mut prepared_by_operation = BTreeMap::new();
        for (value, sequence) in &prepared_files {
            let Some(path) = mutation_file_path(&value.subject) else {
                bail!("Intent operation prepared a non-file mutation");
            };
            if *sequence <= *started_sequence
                || value.tool_call_id.as_deref() != Some(operation.preview.operation_id.as_str())
                || !expected_paths.contains(&path)
                || prepared_by_operation
                    .insert(value.operation_id.clone(), (path, value, *sequence))
                    .is_some()
            {
                bail!("Intent operation contains invalid per-file prepare evidence");
            }
        }
        let committed = self.committed.get(batch_id).cloned().unwrap_or_default();
        let reconciled = self.reconciled.get(batch_id).cloned().unwrap_or_default();
        let mut applied = BTreeSet::new();
        let mut conflicted = BTreeSet::new();
        let mut snapshots = Vec::<(u64, String)>::new();
        for (value, sequence) in committed {
            let Some((path, _, prepared_sequence)) = prepared_by_operation.get(&value.operation_id)
            else {
                bail!("Intent operation commit has no matching prepare");
            };
            if sequence <= *prepared_sequence
                || mutation_file_path(&value.committed_subject).as_ref() != Some(path)
            {
                bail!("Intent operation commit does not match its prepare");
            }
            applied.insert(value.operation_id);
            snapshots.push((sequence, value.workspace_snapshot_id));
        }
        for (value, sequence) in reconciled {
            let Some((_, _, prepared_sequence)) = prepared_by_operation.get(&value.operation_id)
            else {
                bail!("Intent operation reconciliation has no matching prepare");
            };
            if sequence <= *prepared_sequence {
                bail!("Intent operation reconciliation precedes its prepare");
            }
            match (value.observed_state, value.resolution) {
                (MutationObservedState::AppliedAsIntended, MutationResolution::MarkCommitted) => {
                    applied.insert(value.operation_id);
                    if let Some(snapshot_id) = value.workspace_snapshot_id {
                        snapshots.push((sequence, snapshot_id));
                    }
                }
                (MutationObservedState::AppliedDifferently, MutationResolution::MarkConflict)
                | (MutationObservedState::Unknown, MutationResolution::MarkUnknownDirty) => {
                    conflicted.insert(value.operation_id);
                }
                (MutationObservedState::NotApplied, MutationResolution::MarkNotApplied) => {}
                _ => bail!("Intent operation reconciliation has an invalid state/resolution pair"),
            }
        }
        let terminal = self.finished.get(batch_id);
        if let Some((finished, terminal_sequence)) = terminal {
            if *terminal_sequence <= *started_sequence
                || finished.prepared_digest.as_deref() != Some(prepared.preview_digest.as_str())
                || finished.approval_identity.as_deref()
                    != Some(prepared.approval_authority_id.as_str())
                || finished.policy_fingerprint.as_deref()
                    != Some(prepared.permission_policy_digest.as_str())
            {
                bail!("Intent operation batch terminal does not match prepared authority");
            }
            let terminal_committed = finished
                .committed_operations
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            if !terminal_committed.is_subset(&applied) {
                bail!("Intent operation batch claims commits without terminal file evidence");
            }
        }
        snapshots.sort();
        let result_snapshot_id = snapshots.last().map(|(_, id)| id.clone());
        if conflicted.is_empty()
            && prepared_by_operation.len() == expected_paths.len()
            && applied.len() == expected_paths.len()
        {
            let operation_ids = applied.iter().cloned().collect();
            let result_snapshot_id = result_snapshot_id
                .clone()
                .context("applied Intent operation has no result snapshot")?;
            if terminal.is_none() {
                return Ok(BatchOutcome::FullyAppliedUnfinished {
                    operation_ids,
                    result_snapshot_id,
                });
            }
            if terminal.is_some_and(|(finished, _)| {
                finished.status == MutationBatchStatus::Applied
                    && finished.committed_operations.len() == expected_paths.len()
                    && finished.failed_operations.is_empty()
            }) {
                return Ok(BatchOutcome::Applied {
                    operation_ids,
                    result_snapshot_id,
                });
            }
        }
        if !conflicted.is_empty() {
            return Ok(BatchOutcome::Conflicted {
                applied: applied.into_iter().collect(),
                result_snapshot_id,
            });
        }
        if applied.is_empty() {
            Ok(BatchOutcome::Interrupted { result_snapshot_id })
        } else {
            Ok(BatchOutcome::Partial {
                applied: applied.into_iter().collect(),
                result_snapshot_id,
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BatchOutcome {
    NoBatch,
    Applied {
        operation_ids: Vec<String>,
        result_snapshot_id: String,
    },
    FullyAppliedUnfinished {
        operation_ids: Vec<String>,
        result_snapshot_id: String,
    },
    Partial {
        applied: Vec<String>,
        result_snapshot_id: Option<String>,
    },
    Conflicted {
        applied: Vec<String>,
        result_snapshot_id: Option<String>,
    },
    Interrupted {
        result_snapshot_id: Option<String>,
    },
}

#[derive(Debug, Clone, Default)]
struct OperationReplay {
    operations: BTreeMap<IntentOperationId, OperationRecord>,
    conflicts: Vec<IntentConflictV1>,
    batches: BatchEvidence,
}

impl OperationReplay {
    fn from_records(
        records: &[SessionStreamRecord],
        admission: &IntentStackProjectionV1,
        layers: &IntentLayerProjectionV1,
    ) -> Result<Self> {
        let has_operation_events = records.iter().any(|record| {
            matches!(
                record.stored_event().event_kind(),
                Some(
                    DurableEventType::IntentOperationRequested
                        | DurableEventType::IntentOperationPrepared
                        | DurableEventType::IntentOperationResolved
                        | DurableEventType::IntentConflictRecorded
                )
            )
        });
        if admission.latest_accepted_plan().is_none() {
            if has_operation_events {
                bail!("Intent operation events exist without an accepted IntentPlan");
            }
            return Ok(Self::default());
        }
        let mut replay = Self {
            batches: BatchEvidence::from_records(records)?,
            ..Self::default()
        };
        let mut dropped_before = BTreeSet::new();
        for record in records {
            let event = record.stored_event();
            if !matches!(
                event.event_kind(),
                Some(
                    DurableEventType::IntentOperationRequested
                        | DurableEventType::IntentOperationPrepared
                        | DurableEventType::IntentOperationResolved
                        | DurableEventType::IntentConflictRecorded
                )
            ) {
                continue;
            }
            let intent_event = decode_operation_event(event.clone())?;
            match intent_event {
                IntentEventV1::OperationRequested { preview, .. } => {
                    let accepted = admission.accepted_plan(preview.stack_version).context(
                        "Intent operation request references an unaccepted plan version",
                    )?;
                    if preview.operation_kind != IntentOperationKind::Drop
                        || preview.stack_id != accepted.plan.stack_id
                        || accepted.accepted_stream_sequence >= event.stream_sequence
                        || preview.target_intents.len() != 1
                        || !accepted
                            .plan
                            .intents
                            .iter()
                            .any(|intent| intent.intent_ref == preview.target_intents[0])
                    {
                        bail!("Intent operation request references unsupported or stale authority");
                    }
                    validate_requested_preview(
                        &preview,
                        accepted,
                        layers,
                        &dropped_before,
                        event.stream_sequence,
                    )?;
                    if replay
                        .operations
                        .insert(
                            preview.operation_id.clone(),
                            OperationRecord {
                                preview,
                                requested_sequence: event.stream_sequence,
                                prepared: None,
                                prepared_sequence: None,
                                resolved: None,
                                resolved_sequence: None,
                            },
                        )
                        .is_some()
                    {
                        bail!("Intent operation was requested more than once");
                    }
                }
                IntentEventV1::OperationPrepared {
                    operation_id,
                    stack_id,
                    stack_version,
                    preview_digest,
                    artifact_manifest_digest,
                    workspace_revision,
                    permission_policy_digest,
                    approval_authority_id,
                    expires_at_ms: _,
                    mutation_batch_id,
                    ..
                } => {
                    let operation = replay
                        .operations
                        .get_mut(&operation_id)
                        .context("Intent operation prepare precedes its request")?;
                    let target = &operation.preview.target_intents[0];
                    let layer = layers
                        .latest_layer_for(target)
                        .context("Intent operation prepare has no target layer")?;
                    if event.stream_sequence <= operation.requested_sequence
                        || operation.prepared.is_some()
                        || operation.resolved.is_some()
                        || stack_id != operation.preview.stack_id
                        || stack_version != operation.preview.stack_version
                        || preview_digest != operation.preview.preview_digest
                        || workspace_revision != operation.preview.workspace_revision
                        || artifact_manifest_digest != layer.layer_manifest.artifact_manifest_digest
                    {
                        bail!("Intent operation prepare does not match its exact request");
                    }
                    operation.prepared = Some(PreparedOperation {
                        preview_digest,
                        permission_policy_digest,
                        approval_authority_id,
                        mutation_batch_id,
                    });
                    operation.prepared_sequence = Some(event.stream_sequence);
                }
                IntentEventV1::OperationResolved {
                    operation_id,
                    resolution,
                    mutation_batch_id,
                    result_snapshot_id,
                    error_code,
                    ..
                } => {
                    let operation = replay
                        .operations
                        .get_mut(&operation_id)
                        .context("Intent operation result precedes its request")?;
                    if operation.resolved.is_some()
                        || event.stream_sequence <= operation.requested_sequence
                    {
                        bail!("Intent operation has a duplicate or out-of-order terminal");
                    }
                    if matches!(
                        resolution,
                        IntentOperationResolution::Committed
                            | IntentOperationResolution::PartiallyApplied
                            | IntentOperationResolution::Interrupted
                    ) && operation.prepared.is_none()
                    {
                        bail!("effectful Intent operation terminal has no prepared authority");
                    }
                    if let Some(prepared) = &operation.prepared
                        && mutation_batch_id
                            .as_deref()
                            .is_some_and(|batch| batch != prepared.mutation_batch_id)
                    {
                        bail!("Intent operation terminal references another mutation batch");
                    }
                    operation.resolved = Some(ResolvedOperation {
                        resolution,
                        mutation_batch_id,
                        result_snapshot_id,
                        error_code,
                    });
                    operation.resolved_sequence = Some(event.stream_sequence);
                    if resolution == IntentOperationResolution::Committed {
                        dropped_before.extend(operation.preview.target_intents.iter().cloned());
                    }
                }
                IntentEventV1::ConflictRecorded {
                    stack_id,
                    stack_version,
                    conflict,
                    ..
                } => {
                    let accepted = admission
                        .accepted_plan(stack_version)
                        .context("Intent conflict references an unaccepted plan version")?;
                    if stack_id != accepted.plan.stack_id
                        || accepted.accepted_stream_sequence >= event.stream_sequence
                    {
                        bail!("Intent conflict references another stack version");
                    }
                    replay.conflicts.push(conflict);
                }
                _ => bail!("R51.4 wire type carried another Intent payload"),
            }
        }
        replay.validate_terminals()?;
        Ok(replay)
    }

    fn validate_terminals(&self) -> Result<()> {
        for operation in self.operations.values() {
            let Some(resolved) = &operation.resolved else {
                continue;
            };
            match resolved.resolution {
                IntentOperationResolution::Committed => {
                    let prepared = operation
                        .prepared
                        .as_ref()
                        .context("committed Intent operation has no prepare")?;
                    let BatchOutcome::Applied {
                        result_snapshot_id, ..
                    } = self.batches.outcome(operation, prepared)?
                    else {
                        bail!("committed Intent operation has no fully applied mutation batch");
                    };
                    if resolved.result_snapshot_id.as_deref() != Some(result_snapshot_id.as_str()) {
                        bail!(
                            "committed Intent operation result snapshot does not match its batch"
                        );
                    }
                    if self
                        .batches
                        .finished
                        .get(&prepared.mutation_batch_id)
                        .is_none_or(|(_, sequence)| {
                            operation
                                .resolved_sequence
                                .is_none_or(|resolved_sequence| *sequence >= resolved_sequence)
                        })
                    {
                        bail!("committed Intent operation terminal precedes its batch terminal");
                    }
                }
                IntentOperationResolution::Cancelled => {
                    if let Some(prepared) = &operation.prepared
                        && self.batches.has_file_evidence(&prepared.mutation_batch_id)
                    {
                        bail!("Intent operation was cancelled after file preparation began");
                    }
                }
                IntentOperationResolution::Rejected => {
                    if operation.prepared.is_some() {
                        bail!("rejected Intent operation cannot have prepared mutation authority");
                    }
                }
                IntentOperationResolution::Conflicted => {}
                IntentOperationResolution::PartiallyApplied => {
                    let prepared = operation
                        .prepared
                        .as_ref()
                        .context("partial Intent operation has no prepare")?;
                    if !matches!(
                        self.batches.outcome(operation, prepared)?,
                        BatchOutcome::Partial { .. } | BatchOutcome::Conflicted { .. }
                    ) {
                        bail!("partial Intent operation has no partial/conflicting batch evidence");
                    }
                }
                IntentOperationResolution::Interrupted => {
                    if let Some(prepared) = &operation.prepared
                        && matches!(
                            self.batches.outcome(operation, prepared)?,
                            BatchOutcome::Applied { .. }
                                | BatchOutcome::FullyAppliedUnfinished { .. }
                        )
                    {
                        bail!("fully applied Intent operation cannot resolve as interrupted");
                    }
                }
            }
        }
        Ok(())
    }

    fn public_projection(&self) -> IntentOperationProjectionV1 {
        let mut projection = IntentOperationProjectionV1 {
            conflicts: self.conflicts.clone(),
            ..IntentOperationProjectionV1::default()
        };
        for (operation_id, operation) in &self.operations {
            let state = operation_state(operation, &self.batches);
            if state == IntentOperationStateV1::Committed {
                projection
                    .dropped_intents
                    .extend(operation.preview.target_intents.iter().cloned());
            }
            projection.operations.insert(
                operation_id.clone(),
                IntentOperationSummaryV1 {
                    operation_id: operation_id.clone(),
                    operation_kind: operation.preview.operation_kind,
                    target_intents: operation.preview.target_intents.clone(),
                    state,
                    mutation_batch_id: operation
                        .resolved
                        .as_ref()
                        .and_then(|resolved| resolved.mutation_batch_id.clone())
                        .or_else(|| {
                            operation
                                .prepared
                                .as_ref()
                                .map(|prepared| prepared.mutation_batch_id.clone())
                        }),
                    result_snapshot_id: operation
                        .resolved
                        .as_ref()
                        .and_then(|resolved| resolved.result_snapshot_id.clone()),
                    error_code: operation
                        .resolved
                        .as_ref()
                        .and_then(|resolved| resolved.error_code),
                },
            );
        }
        projection
    }
}

fn operation_state(operation: &OperationRecord, batches: &BatchEvidence) -> IntentOperationStateV1 {
    if let Some(resolved) = &operation.resolved {
        return match resolved.resolution {
            IntentOperationResolution::Committed => IntentOperationStateV1::Committed,
            IntentOperationResolution::Rejected => IntentOperationStateV1::Rejected,
            IntentOperationResolution::Cancelled => IntentOperationStateV1::Cancelled,
            IntentOperationResolution::Conflicted => IntentOperationStateV1::Conflicted,
            IntentOperationResolution::PartiallyApplied => IntentOperationStateV1::PartiallyApplied,
            IntentOperationResolution::Interrupted => IntentOperationStateV1::Interrupted,
        };
    }
    if let Some(prepared) = &operation.prepared {
        if batches.has_file_evidence(&prepared.mutation_batch_id) {
            IntentOperationStateV1::Applying
        } else {
            IntentOperationStateV1::Prepared
        }
    } else {
        IntentOperationStateV1::Requested
    }
}

fn validate_requested_preview(
    preview: &IntentOperationPreviewV1,
    accepted: &crate::AcceptedIntentPlanProjectionV1,
    layers: &IntentLayerProjectionV1,
    dropped_before: &BTreeSet<IntentVersionRef>,
    request_sequence: u64,
) -> Result<()> {
    let target = &preview.target_intents[0];
    let definition = accepted
        .plan
        .intents
        .iter()
        .find(|definition| &definition.intent_ref == target)
        .context("Intent operation request target is not accepted")?;
    let expected_leaf = !accepted.plan.intents.iter().any(|candidate| {
        candidate.intent_ref != *target
            && !dropped_before.contains(&candidate.intent_ref)
            && candidate.depends_on.contains(&target.intent_id)
    });
    if preview.target_is_leaf != expected_leaf {
        bail!("Intent operation request carries a stale dependency result");
    }
    let expected_retained = accepted
        .plan
        .intents
        .iter()
        .filter(|candidate| {
            candidate.intent_ref != *target && !dropped_before.contains(&candidate.intent_ref)
        })
        .map(|candidate| candidate.intent_ref.clone())
        .collect::<Vec<_>>();
    if preview.retained_intents != expected_retained {
        bail!("Intent operation request carries a forged retained-intent set");
    }
    let layer = layers.layer_order.iter().rev().find_map(|execution_id| {
        layers.layers.get(execution_id).filter(|layer| {
            layer.layer_stream_sequence < request_sequence
                && layer.layer_manifest.core.intent_ref == *target
        })
    });
    let operation_projection = IntentOperationProjectionV1 {
        dropped_intents: dropped_before.clone(),
        ..IntentOperationProjectionV1::default()
    };
    let expected_operation_id = operation_id_for_preview_target(
        accepted,
        layers,
        &operation_projection,
        definition,
        preview.workspace_revision,
        request_sequence.saturating_sub(1),
    )
    .context("failed to reconstruct Intent operation id")?;
    if preview.operation_id != expected_operation_id {
        bail!("Intent operation request identity is not runtime-derived");
    }
    if preview.conflicts.is_empty() {
        let layer = layer.context("executable Intent operation request has no prior layer")?;
        let mut expected = BTreeMap::<String, BTreeSet<IntentArtifactId>>::new();
        for artifact in &layer.artifacts {
            let BoundedIntentArtifactSubjectV1::FileHunk {
                normalized_relative_path,
                ..
            } = &artifact.binding.subject
            else {
                bail!("executable Intent operation layer contains a non-file artifact");
            };
            expected
                .entry(normalized_relative_path.clone())
                .or_default()
                .insert(artifact.binding.artifact_id.clone());
        }
        let actual = preview
            .file_effects
            .iter()
            .map(|file| {
                (
                    file.normalized_relative_path.clone(),
                    file.artifact_ids.iter().cloned().collect::<BTreeSet<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        if expected != actual || preview.file_effects.len() != actual.len() {
            bail!("Intent operation request file set does not match its exact layer");
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct ResolvedDropFile {
    path: String,
    absolute_path: PathBuf,
    expected_hash: Option<String>,
    target: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct ResolvedDrop {
    preview: IntentOperationPreviewV1,
    artifact_manifest_digest: IntentDigest,
    source_frontier_sequence: u64,
    files: Vec<ResolvedDropFile>,
}

#[derive(Debug, Clone)]
struct PreparedDropFile {
    file: ResolvedDropFile,
    mutation: PreparedFileMutation,
}

/// Builds a read-only exact drop preview from durable layer authority and current disk state.
pub fn preview_intent_drop(
    session: &Session,
    workspace_root: impl AsRef<Path>,
    intent_ref: &IntentVersionRef,
) -> Result<IntentOperationPreviewV1> {
    resolve_drop(session, workspace_root.as_ref(), intent_ref).map(|resolved| resolved.preview)
}

/// Applies one exact drop under a workspace lease and RFC-0002 mutation batch.
pub fn execute_intent_drop(
    session: &Session,
    workspace_root: impl AsRef<Path>,
    request: &IntentDropRequestV1,
    authority: &IntentOperationAuthorityV1,
    safe_reason: impl Into<String>,
) -> Result<IntentOperationExecutionV1> {
    let safe_reason = safe_reason.into();
    if safe_reason.trim().is_empty() || safe_reason.len() > crate::MAX_INTENT_REASON_BYTES {
        bail!("intent drop reason is empty or too long");
    }
    let workspace_root = fs::canonicalize(workspace_root.as_ref()).with_context(|| {
        format!(
            "failed to canonicalize Intent operation workspace {}",
            workspace_root.as_ref().display()
        )
    })?;
    let store = session
        .durable_store()
        .context("Intent drop requires a durable session")?;
    let recorder = session
        .mutation_event_recorder()
        .context("Intent drop requires mutation artifacts")?;
    let batch_id = intent_drop_batch_id(&request.operation_id);
    let coordinator = recorder.coordinator_with_workspace_lease(
        &workspace_root,
        request.operation_id.as_str().to_owned(),
        Some(batch_id.clone()),
    )?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let admission = IntentStackProjectionV1::from_records(&records)?;
    let lineage = crate::IntentLineageProjectionV1::from_records(&records, &admission)?;
    let mut layers = IntentLayerProjectionV1::from_records(&records, &admission, &lineage)?;
    layers.refresh_artifact_availability(&recorder)?;
    let replay = OperationReplay::from_records(&records, &admission, &layers)?;
    if let Some(existing) = replay.operations.get(&request.operation_id) {
        if existing.preview.preview_digest != request.preview_digest {
            bail!("Intent operation id is already bound to another preview");
        }
        if let Some(resolved) = &existing.resolved {
            return execution_from_existing(existing, resolved, &replay.batches);
        }
        if existing.prepared.is_some() {
            bail!("prepared Intent operation requires reconciliation and will not be replayed");
        }
    }
    let current_workspace_revision = recorder.current_workspace_revision(&workspace_root)?;
    let source_frontier_sequence = records
        .last()
        .map(|record| record.stored_event().stream_sequence)
        .unwrap_or_default();
    let operation_projection = replay.public_projection();
    let target = replay
        .operations
        .get(&request.operation_id)
        .map(|operation| operation.preview.target_intents[0].clone())
        .or_else(|| {
            admission.latest_accepted_plan().and_then(|accepted| {
                accepted
                    .plan
                    .intents
                    .iter()
                    .find(|intent| {
                        operation_id_for_preview_target(
                            accepted,
                            &layers,
                            &operation_projection,
                            intent,
                            current_workspace_revision,
                            source_frontier_sequence,
                        )
                        .as_ref()
                            == Some(&request.operation_id)
                    })
                    .map(|intent| intent.intent_ref.clone())
            })
        })
        .context("Intent drop request references an unknown operation")?;
    let resolved = resolve_drop_from_parts(
        &records,
        &workspace_root,
        &recorder,
        &admission,
        &lineage,
        &layers,
        &replay.public_projection(),
        &target,
    )?;
    validate_drop_request(request, &resolved.preview)?;
    append_operation_requested(
        &store,
        &resolved.preview,
        &safe_reason,
        resolved.source_frontier_sequence,
    )?;

    if !resolved.preview.conflicts.is_empty() {
        append_conflicted_operation(&store, &resolved.preview)?;
        return Ok(IntentOperationExecutionV1 {
            preview: resolved.preview,
            resolution: IntentOperationResolution::Conflicted,
            mutation_batch_id: None,
            committed_operation_ids: Vec::new(),
            result_snapshot_id: None,
            error_code: Some(IntentOperationErrorCode::IntentStateConflict),
        });
    }
    if !authority.is_current(now_ms()?) {
        append_operation_resolved(
            &store,
            &resolved.preview.operation_id,
            IntentOperationResolution::Rejected,
            None,
            None,
            Some(IntentOperationErrorCode::PermissionDenied),
        )?;
        return Ok(IntentOperationExecutionV1 {
            preview: resolved.preview,
            resolution: IntentOperationResolution::Rejected,
            mutation_batch_id: None,
            committed_operation_ids: Vec::new(),
            result_snapshot_id: None,
            error_code: Some(IntentOperationErrorCode::PermissionDenied),
        });
    }

    append_operation_prepared(&store, &resolved, authority, &batch_id)?;
    let expected_subjects = resolved
        .files
        .iter()
        .map(|file| MutationSubject::File {
            path: PathBuf::from(&file.path),
            file_type: FileType::File,
        })
        .collect::<Vec<_>>();
    recorder.append_bound_batch_started(
        &batch_id,
        resolved.preview.operation_id.as_str(),
        &expected_subjects,
        Some(resolved.preview.preview_digest.as_str()),
        Some(&authority.approval_authority_id),
        Some(authority.permission_policy_digest.as_str()),
    )?;

    let prepared_files = match prepare_drop_files(&coordinator, &resolved.files) {
        Ok(prepared_files) => prepared_files,
        Err((error, prepared_operation_ids)) => {
            return finish_failed_drop(
                &store,
                &recorder,
                &resolved.preview,
                authority,
                &batch_id,
                &[],
                &prepared_operation_ids,
                None,
                error,
            );
        }
    };
    let mut committed_operation_ids = Vec::with_capacity(prepared_files.len());
    let mut result_snapshot_id = None;
    for prepared in prepared_files {
        let committed = match prepared.file.target.as_deref() {
            Some(content) => coordinator.commit_write(&prepared.mutation, content),
            None => coordinator.commit_delete(&prepared.mutation),
        };
        match committed {
            Ok(committed) => {
                committed_operation_ids.push(committed.operation_id);
                result_snapshot_id = Some(committed.workspace_snapshot_id);
            }
            Err(error) => {
                let reconciled = coordinator
                    .reconcile_prepared_file_from_disk(&prepared.mutation)
                    .context("failed to reconcile interrupted Intent drop file")?;
                let value: MutationReconciled = serde_json::from_value(reconciled.payload)
                    .context("failed to decode just-appended Intent reconciliation")?;
                if value.resolution == MutationResolution::MarkCommitted {
                    committed_operation_ids.push(prepared.mutation.operation_id.clone());
                    result_snapshot_id = value.workspace_snapshot_id;
                }
                let failed_operation_ids = [prepared.mutation.operation_id];
                return finish_failed_drop(
                    &store,
                    &recorder,
                    &resolved.preview,
                    authority,
                    &batch_id,
                    &committed_operation_ids,
                    &failed_operation_ids,
                    result_snapshot_id.clone(),
                    error,
                );
            }
        }
    }
    recorder.append_bound_batch_finished(
        &batch_id,
        MutationBatchStatus::Applied,
        &committed_operation_ids,
        &[],
        &[],
        &[],
        Some(resolved.preview.preview_digest.as_str()),
        Some(&authority.approval_authority_id),
        Some(authority.permission_policy_digest.as_str()),
    )?;
    let result_snapshot_id =
        result_snapshot_id.context("Intent drop completed without a result snapshot")?;
    append_operation_resolved(
        &store,
        &resolved.preview.operation_id,
        IntentOperationResolution::Committed,
        Some(&batch_id),
        Some(&result_snapshot_id),
        None,
    )?;
    Ok(IntentOperationExecutionV1 {
        preview: resolved.preview,
        resolution: IntentOperationResolution::Committed,
        mutation_batch_id: Some(batch_id),
        committed_operation_ids,
        result_snapshot_id: Some(result_snapshot_id),
        error_code: None,
    })
}

fn prepare_drop_files(
    coordinator: &MutationCoordinator,
    files: &[ResolvedDropFile],
) -> std::result::Result<Vec<PreparedDropFile>, (anyhow::Error, Vec<String>)> {
    let mut prepared_files = Vec::with_capacity(files.len());
    for file in files {
        let intended_hash = file.target.as_deref().map(crate::bytes_hash);
        match coordinator.prepare_file_expected(
            PathBuf::from(&file.path),
            file.absolute_path.clone(),
            file.expected_hash.clone(),
            intended_hash,
        ) {
            Ok(mutation) => prepared_files.push(PreparedDropFile {
                file: file.clone(),
                mutation,
            }),
            Err(error) => {
                let prepared_operation_ids = prepared_files
                    .iter()
                    .map(|prepared: &PreparedDropFile| prepared.mutation.operation_id.clone())
                    .collect();
                return Err((error, prepared_operation_ids));
            }
        }
    }
    Ok(prepared_files)
}

/// Cancels a requested/prepared operation only while no per-file prepare evidence exists.
pub fn cancel_intent_operation(
    session: &Session,
    operation_id: &IntentOperationId,
) -> Result<bool> {
    let store = session
        .durable_store()
        .context("Intent operation cancellation requires a durable session")?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let admission = IntentStackProjectionV1::from_records(&records)?;
    let lineage = crate::IntentLineageProjectionV1::from_records(&records, &admission)?;
    let layers = IntentLayerProjectionV1::from_records(&records, &admission, &lineage)?;
    let replay = OperationReplay::from_records(&records, &admission, &layers)?;
    let operation = replay
        .operations
        .get(operation_id)
        .context("cannot cancel an unknown Intent operation")?;
    if operation.resolved.is_some() {
        return Ok(false);
    }
    if let Some(prepared) = &operation.prepared
        && replay
            .batches
            .has_file_evidence(&prepared.mutation_batch_id)
    {
        bail!("cannot cancel an Intent operation after file preparation began");
    }
    append_operation_resolved(
        &store,
        operation_id,
        IntentOperationResolution::Cancelled,
        operation
            .prepared
            .as_ref()
            .map(|prepared| prepared.mutation_batch_id.as_str()),
        None,
        None,
    )?;
    Ok(true)
}

/// Reconciles interrupted Intent operations without replaying any file write.
pub fn reconcile_intent_operations(
    session: &mut Session,
    workspace_root: impl AsRef<Path>,
) -> Result<Vec<IntentOperationId>> {
    let workspace_root = fs::canonicalize(workspace_root.as_ref()).with_context(|| {
        format!(
            "failed to canonicalize Intent recovery workspace {}",
            workspace_root.as_ref().display()
        )
    })?;
    session.reconcile_prepared_mutations(&workspace_root)?;
    let store = session
        .durable_store()
        .context("Intent operation recovery requires a durable session")?;
    let recorder = session
        .mutation_event_recorder()
        .context("Intent operation recovery requires mutation evidence")?;
    let _lease = recorder.coordinator_with_workspace_lease(
        &workspace_root,
        "intent-operation-recovery",
        None,
    )?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let admission = IntentStackProjectionV1::from_records(&records)?;
    let accepted = admission
        .latest_accepted_plan()
        .context("Intent operation recovery requires an accepted plan")?;
    if stable_workspace_id(&workspace_root)? != accepted.plan.workspace_id {
        bail!("Intent operation recovery workspace does not match the accepted plan");
    }
    let lineage = crate::IntentLineageProjectionV1::from_records(&records, &admission)?;
    let layers = IntentLayerProjectionV1::from_records(&records, &admission, &lineage)?;
    let replay = OperationReplay::from_records(&records, &admission, &layers)?;
    let mut reconciled = Vec::new();
    for (operation_id, operation) in &replay.operations {
        if operation.resolved.is_some() {
            continue;
        }
        let Some(prepared) = &operation.prepared else {
            continue;
        };
        let outcome = replay.batches.outcome(operation, prepared)?;
        let (resolution, error_code, applied, result_snapshot_id, batch_status) = match outcome {
            BatchOutcome::Applied {
                operation_ids,
                result_snapshot_id,
            } => (
                IntentOperationResolution::Committed,
                None,
                operation_ids,
                Some(result_snapshot_id),
                MutationBatchStatus::Applied,
            ),
            BatchOutcome::FullyAppliedUnfinished {
                operation_ids,
                result_snapshot_id,
            } => (
                IntentOperationResolution::Committed,
                None,
                operation_ids,
                Some(result_snapshot_id),
                MutationBatchStatus::Applied,
            ),
            BatchOutcome::Partial {
                applied,
                result_snapshot_id,
            } => (
                IntentOperationResolution::PartiallyApplied,
                Some(IntentOperationErrorCode::PartialApplication),
                applied,
                result_snapshot_id,
                MutationBatchStatus::PartiallyApplied,
            ),
            BatchOutcome::Conflicted {
                applied,
                result_snapshot_id,
            } => (
                IntentOperationResolution::PartiallyApplied,
                Some(IntentOperationErrorCode::ReconciliationRequired),
                applied,
                result_snapshot_id,
                MutationBatchStatus::PartiallyApplied,
            ),
            BatchOutcome::Interrupted { result_snapshot_id } => (
                IntentOperationResolution::Interrupted,
                Some(IntentOperationErrorCode::ReconciliationRequired),
                Vec::new(),
                result_snapshot_id,
                MutationBatchStatus::Failed,
            ),
            BatchOutcome::NoBatch => (
                IntentOperationResolution::Interrupted,
                Some(IntentOperationErrorCode::ReconciliationRequired),
                Vec::new(),
                None,
                MutationBatchStatus::Failed,
            ),
        };
        if !replay
            .batches
            .finished
            .contains_key(&prepared.mutation_batch_id)
            && replay
                .batches
                .started
                .contains_key(&prepared.mutation_batch_id)
        {
            recorder.append_bound_batch_finished(
                &prepared.mutation_batch_id,
                batch_status,
                &applied,
                &[],
                &[],
                &[],
                Some(prepared.preview_digest.as_str()),
                Some(&prepared.approval_authority_id),
                Some(prepared.permission_policy_digest.as_str()),
            )?;
        }
        append_operation_resolved(
            &store,
            operation_id,
            resolution,
            Some(&prepared.mutation_batch_id),
            result_snapshot_id.as_deref(),
            error_code,
        )?;
        reconciled.push(operation_id.clone());
    }
    Ok(reconciled)
}

fn resolve_drop(
    session: &Session,
    workspace_root: &Path,
    intent_ref: &IntentVersionRef,
) -> Result<ResolvedDrop> {
    let workspace_root = fs::canonicalize(workspace_root).with_context(|| {
        format!(
            "failed to canonicalize Intent preview workspace {}",
            workspace_root.display()
        )
    })?;
    let store = session
        .durable_store()
        .context("Intent drop preview requires a durable session")?;
    let recorder = session
        .mutation_event_recorder()
        .context("Intent drop preview requires mutation artifacts")?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let admission = IntentStackProjectionV1::from_records(&records)?;
    let lineage = crate::IntentLineageProjectionV1::from_records(&records, &admission)?;
    let mut layers = IntentLayerProjectionV1::from_records(&records, &admission, &lineage)?;
    layers.refresh_artifact_availability(&recorder)?;
    let operations = IntentOperationProjectionV1::from_records(&records, &admission, &layers)?;
    resolve_drop_from_parts(
        &records,
        &workspace_root,
        &recorder,
        &admission,
        &lineage,
        &layers,
        &operations,
        intent_ref,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_drop_from_parts(
    records: &[SessionStreamRecord],
    workspace_root: &Path,
    recorder: &MutationEventRecorder,
    admission: &IntentStackProjectionV1,
    lineage: &crate::IntentLineageProjectionV1,
    layers: &IntentLayerProjectionV1,
    operations: &IntentOperationProjectionV1,
    intent_ref: &IntentVersionRef,
) -> Result<ResolvedDrop> {
    let accepted = admission
        .latest_accepted_plan()
        .context("Intent drop requires an accepted IntentPlan")?;
    if stable_workspace_id(workspace_root)? != accepted.plan.workspace_id {
        bail!("Intent drop workspace identity does not match the accepted plan");
    }
    let definition = accepted
        .plan
        .intents
        .iter()
        .find(|intent| &intent.intent_ref == intent_ref)
        .context("Intent drop target is not in the accepted plan")?;
    let source_frontier_sequence = records
        .last()
        .map(|record| record.stored_event().stream_sequence)
        .unwrap_or_default();
    let workspace_revision = recorder.current_workspace_revision(workspace_root)?;
    let operation_id = operation_id_for_preview_target(
        accepted,
        layers,
        operations,
        definition,
        workspace_revision,
        source_frontier_sequence,
    )
    .context("failed to derive Intent drop operation identity")?;
    let mut conflicts = Vec::new();
    if operations.is_dropped(intent_ref) {
        conflicts.push(intent_conflict(
            IntentOperationErrorCode::IntentStateConflict,
            Some(intent_ref.clone()),
            None,
            "Intent is already dropped",
        ));
    }
    let target_is_leaf = !accepted.plan.intents.iter().any(|candidate| {
        candidate.intent_ref != *intent_ref
            && !operations.is_dropped(&candidate.intent_ref)
            && candidate.depends_on.contains(&intent_ref.intent_id)
    });
    if !target_is_leaf {
        conflicts.push(intent_conflict(
            IntentOperationErrorCode::TargetNotLeaf,
            Some(intent_ref.clone()),
            None,
            "Active downstream intent depends on this target",
        ));
    }
    let retained_intents = accepted
        .plan
        .intents
        .iter()
        .filter(|intent| {
            &intent.intent_ref != intent_ref && !operations.is_dropped(&intent.intent_ref)
        })
        .map(|intent| intent.intent_ref.clone())
        .collect::<Vec<_>>();
    let verification_impacts = lineage
        .current_system_verification_receipt_ids(intent_ref)
        .into_iter()
        .flat_map(|receipt_id| {
            [
                IntentVerificationImpactV1 {
                    receipt_id: receipt_id.clone(),
                    impact: IntentVerificationImpact::BecomesStale,
                },
                IntentVerificationImpactV1 {
                    receipt_id,
                    impact: IntentVerificationImpact::RerunRequired,
                },
            ]
        })
        .collect::<Vec<_>>();

    let Some(layer) = layers.latest_layer_for(intent_ref) else {
        conflicts.push(intent_conflict(
            IntentOperationErrorCode::MissingExecutionLineage,
            Some(intent_ref.clone()),
            None,
            "Intent has no executable layer",
        ));
        return finish_drop_resolution(
            operation_id,
            accepted,
            intent_ref,
            target_is_leaf,
            workspace_revision,
            source_frontier_sequence,
            Vec::new(),
            retained_intents,
            verification_impacts,
            conflicts,
            zero_intent_digest()?,
            Vec::new(),
        );
    };
    for artifact in &layer.artifacts {
        let (code, reason) = match (artifact.ownership, artifact.availability) {
            (_, availability) if availability != IntentArtifactAvailability::Available => (
                IntentOperationErrorCode::ArtifactUnavailable,
                "Intent artifact is unavailable",
            ),
            (IntentArtifactOwnership::Shared, _) => (
                IntentOperationErrorCode::SharedArtifact,
                "Intent artifact is shared with another active intent",
            ),
            (IntentArtifactOwnership::Unowned, _) => (
                IntentOperationErrorCode::UnownedArtifact,
                "Intent artifact ownership is unproved",
            ),
            (IntentArtifactOwnership::Drifted, _) => (
                IntentOperationErrorCode::DriftedArtifact,
                "Intent artifact no longer matches the workspace",
            ),
            (IntentArtifactOwnership::Exclusive, IntentArtifactAvailability::Available) => continue,
            _ => (
                IntentOperationErrorCode::ArtifactUnavailable,
                "Intent artifact is not executable",
            ),
        };
        conflicts.push(intent_conflict(
            code,
            Some(intent_ref.clone()),
            Some(artifact.binding.artifact_id.clone()),
            reason,
        ));
    }
    let reverse_bytes = match recorder
        .read_immutable_content_artifact(&layer.layer_manifest.core.reverse_patch_artifact_id)
    {
        Ok(bytes) => bytes,
        Err(_) => {
            conflicts.push(intent_conflict(
                IntentOperationErrorCode::ArtifactUnavailable,
                Some(intent_ref.clone()),
                None,
                "Intent reverse patch artifact is unavailable",
            ));
            return finish_drop_resolution(
                operation_id,
                accepted,
                intent_ref,
                target_is_leaf,
                workspace_revision,
                source_frontier_sequence,
                Vec::new(),
                retained_intents,
                verification_impacts,
                conflicts,
                layer.layer_manifest.artifact_manifest_digest.clone(),
                Vec::new(),
            );
        }
    };
    let patch = match decode_exact_intent_patch(&reverse_bytes) {
        Ok(patch) => patch,
        Err(_) => {
            conflicts.push(intent_conflict(
                IntentOperationErrorCode::ArtifactDigestMismatch,
                Some(intent_ref.clone()),
                None,
                "Intent reverse patch artifact is malformed",
            ));
            return finish_drop_resolution(
                operation_id,
                accepted,
                intent_ref,
                target_is_leaf,
                workspace_revision,
                source_frontier_sequence,
                Vec::new(),
                retained_intents,
                verification_impacts,
                conflicts,
                layer.layer_manifest.artifact_manifest_digest.clone(),
                Vec::new(),
            );
        }
    };
    let (file_effects, files) =
        resolve_drop_files(workspace_root, intent_ref, layer, patch, &mut conflicts)?;
    finish_drop_resolution(
        operation_id,
        accepted,
        intent_ref,
        target_is_leaf,
        workspace_revision,
        source_frontier_sequence,
        file_effects,
        retained_intents,
        verification_impacts,
        conflicts,
        layer.layer_manifest.artifact_manifest_digest.clone(),
        files,
    )
}

fn resolve_drop_files(
    workspace_root: &Path,
    intent_ref: &IntentVersionRef,
    layer: &IntentLayerStateV1,
    patch: Vec<ExactIntentPatchFileV1>,
    conflicts: &mut Vec<IntentConflictV1>,
) -> Result<(Vec<IntentOperationFileSummaryV1>, Vec<ResolvedDropFile>)> {
    let mut artifacts_by_path = BTreeMap::<String, Vec<_>>::new();
    for artifact in &layer.artifacts {
        let BoundedIntentArtifactSubjectV1::FileHunk {
            normalized_relative_path,
            ..
        } = &artifact.binding.subject
        else {
            conflicts.push(intent_conflict(
                IntentOperationErrorCode::UnsupportedArtifact,
                Some(intent_ref.clone()),
                Some(artifact.binding.artifact_id.clone()),
                "Intent layer contains a non-file artifact",
            ));
            continue;
        };
        artifacts_by_path
            .entry(normalized_relative_path.clone())
            .or_default()
            .push(artifact);
    }
    let patch_paths = patch
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    if patch_paths != artifacts_by_path.keys().cloned().collect::<BTreeSet<_>>() {
        conflicts.push(intent_conflict(
            IntentOperationErrorCode::ArtifactDigestMismatch,
            Some(intent_ref.clone()),
            None,
            "Intent reverse patch paths do not match the layer manifest",
        ));
    }
    let mut file_effects = Vec::with_capacity(patch.len());
    let mut files = Vec::with_capacity(patch.len());
    for file in patch {
        let Some(artifacts) = artifacts_by_path.get(&file.path) else {
            continue;
        };
        let mut binding_valid = true;
        for artifact in artifacts {
            let BoundedIntentArtifactSubjectV1::FileHunk {
                before_file_digest,
                after_file_digest,
                ..
            } = &artifact.binding.subject
            else {
                continue;
            };
            if after_file_digest != &file.expected_digest
                || file
                    .target
                    .as_deref()
                    .map(IntentContentDigest::from_bytes)
                    .unwrap_or_else(|| IntentContentDigest::from_bytes([]))
                    != *before_file_digest
            {
                binding_valid = false;
            }
        }
        if !binding_valid {
            conflicts.push(intent_conflict(
                IntentOperationErrorCode::ArtifactDigestMismatch,
                Some(intent_ref.clone()),
                artifacts
                    .first()
                    .map(|artifact| artifact.binding.artifact_id.clone()),
                "Intent reverse patch bytes do not match the layer manifest",
            ));
        }
        let action = match (file.expected_present, file.target.is_some()) {
            (true, false) => IntentOperationFileAction::Delete,
            (true, true) => IntentOperationFileAction::Update,
            (false, true) => IntentOperationFileAction::Create,
            (false, false) => {
                conflicts.push(intent_conflict(
                    IntentOperationErrorCode::UnsupportedArtifact,
                    Some(intent_ref.clone()),
                    None,
                    "Intent reverse patch contains a no-op file target",
                ));
                continue;
            }
        };
        let relative = PathBuf::from(&file.path);
        let absolute = workspace_root.join(&relative);
        let metadata = fs::symlink_metadata(&absolute);
        let target_is_plain_file = matches!(
            metadata,
            Ok(ref metadata) if metadata.is_file() && !metadata.file_type().is_symlink()
        );
        let target_is_missing = matches!(
            metadata,
            Err(ref error) if error.kind() == std::io::ErrorKind::NotFound
        );
        let parent_is_plain_directory = absolute.parent().is_some_and(|parent| {
            matches!(
                fs::symlink_metadata(parent),
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink()
            )
        });
        if (file.expected_present && !target_is_plain_file)
            || (!file.expected_present && !target_is_missing)
            || !parent_is_plain_directory
        {
            conflicts.push(intent_conflict(
                IntentOperationErrorCode::WorkspaceOutOfScope,
                Some(intent_ref.clone()),
                None,
                "Intent target is missing, a symlink, or outside a plain workspace parent",
            ));
        }
        let actual_hash = file_content_hash(&absolute).ok().flatten();
        let expected_hash = file
            .expected_present
            .then(|| file.expected_digest.as_str().to_owned());
        if actual_hash != expected_hash {
            conflicts.push(intent_conflict(
                IntentOperationErrorCode::DriftedArtifact,
                Some(intent_ref.clone()),
                None,
                "Intent target content changed after layer materialization",
            ));
        }
        file_effects.push(IntentOperationFileSummaryV1 {
            normalized_relative_path: file.path.clone(),
            action,
            artifact_ids: artifacts
                .iter()
                .map(|artifact| artifact.binding.artifact_id.clone())
                .collect(),
        });
        files.push(ResolvedDropFile {
            path: file.path,
            absolute_path: absolute,
            expected_hash,
            target: file.target,
        });
    }
    file_effects.sort_by(|left, right| {
        left.normalized_relative_path
            .cmp(&right.normalized_relative_path)
    });
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok((file_effects, files))
}

#[allow(clippy::too_many_arguments)]
fn finish_drop_resolution(
    operation_id: IntentOperationId,
    accepted: &crate::AcceptedIntentPlanProjectionV1,
    intent_ref: &IntentVersionRef,
    target_is_leaf: bool,
    workspace_revision: u64,
    source_frontier_sequence: u64,
    file_effects: Vec<IntentOperationFileSummaryV1>,
    retained_intents: Vec<IntentVersionRef>,
    verification_impacts: Vec<IntentVerificationImpactV1>,
    mut conflicts: Vec<IntentConflictV1>,
    artifact_manifest_digest: IntentDigest,
    files: Vec<ResolvedDropFile>,
) -> Result<ResolvedDrop> {
    conflicts.sort_by(|left, right| {
        format!("{:?}", left.code)
            .cmp(&format!("{:?}", right.code))
            .then_with(|| left.safe_reason.cmp(&right.safe_reason))
            .then_with(|| left.artifact_id.cmp(&right.artifact_id))
    });
    let mut preview = IntentOperationPreviewV1 {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        operation_id,
        operation_kind: IntentOperationKind::Drop,
        stack_id: accepted.plan.stack_id.clone(),
        stack_version: accepted.plan.stack_version,
        target_intents: vec![intent_ref.clone()],
        target_is_leaf,
        workspace_revision,
        expires_at_ms: None,
        file_effects,
        retained_intents,
        verification_impacts,
        conflicts,
        preview_digest: zero_intent_digest()?,
    };
    preview.preview_digest = preview.computed_digest()?;
    preview.validate_contract()?;
    Ok(ResolvedDrop {
        preview,
        artifact_manifest_digest,
        source_frontier_sequence,
        files,
    })
}

fn operation_id_for_preview_target(
    accepted: &crate::AcceptedIntentPlanProjectionV1,
    layers: &IntentLayerProjectionV1,
    operations: &IntentOperationProjectionV1,
    definition: &crate::IntentDefinitionV1,
    workspace_revision: u64,
    source_frontier_sequence: u64,
) -> Option<IntentOperationId> {
    let layer_digest = layers
        .latest_layer_for(&definition.intent_ref)
        .map(|layer| layer.layer_manifest.manifest_digest.as_str())
        .unwrap_or("no-layer");
    let dropped = operations.is_dropped(&definition.intent_ref);
    IntentOperationId::new(format!(
        "intent-drop-{}",
        stable_event_uuid(
            "sigil-intent-drop-operation",
            &format!(
                "{}:{}:{}:{}:{}:{}:{}",
                accepted.plan.stack_id.as_str(),
                accepted.plan.stack_version.get(),
                definition.intent_ref.intent_id.as_str(),
                definition.intent_ref.version,
                layer_digest,
                workspace_revision.saturating_add(u64::from(dropped)),
                source_frontier_sequence,
            ),
        )
    ))
    .ok()
}

fn validate_drop_request(
    request: &IntentDropRequestV1,
    preview: &IntentOperationPreviewV1,
) -> Result<()> {
    if request.operation_id != preview.operation_id {
        bail!("Intent drop operation id is stale");
    }
    if request.stack_version != preview.stack_version {
        bail!("Intent drop stack version is stale");
    }
    if request.preview_digest != preview.preview_digest {
        bail!("Intent drop preview digest is stale");
    }
    Ok(())
}

fn append_operation_requested(
    store: &JsonlSessionStore,
    preview: &IntentOperationPreviewV1,
    safe_reason: &str,
    source_frontier_sequence: u64,
) -> Result<()> {
    let event = IntentEventV1::OperationRequested {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        preview: preview.clone(),
        safe_reason: safe_reason.to_owned(),
    };
    event.validate_contract()?;
    let expected = event.clone();
    let operation_id = preview.operation_id.clone();
    store.append_events_and_session_entries_if(
        vec![(
            DurableEventType::IntentOperationRequested,
            EventClass::Critical,
            serde_json::to_value(event)?,
        )],
        &[],
        move |records| {
            let current_frontier = records
                .last()
                .map(|record| record.stored_event().stream_sequence)
                .unwrap_or_default();
            if current_frontier != source_frontier_sequence {
                bail!("Intent operation preview frontier changed before request");
            }
            for record in records {
                let stored = record.stored_event();
                if stored.event_kind() != Some(DurableEventType::IntentOperationRequested)
                    || operation_event_id(stored).as_ref() != Some(&operation_id)
                {
                    continue;
                }
                let existing = decode_operation_event(stored.clone())?;
                if existing != expected {
                    bail!("Intent operation request identity is already bound to another payload");
                }
                return Ok(false);
            }
            Ok(true)
        },
    )?;
    Ok(())
}

fn append_operation_prepared(
    store: &JsonlSessionStore,
    resolved: &ResolvedDrop,
    authority: &IntentOperationAuthorityV1,
    batch_id: &str,
) -> Result<()> {
    let event = IntentEventV1::OperationPrepared {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        operation_id: resolved.preview.operation_id.clone(),
        stack_id: resolved.preview.stack_id.clone(),
        stack_version: resolved.preview.stack_version,
        preview_digest: resolved.preview.preview_digest.clone(),
        artifact_manifest_digest: resolved.artifact_manifest_digest.clone(),
        workspace_revision: resolved.preview.workspace_revision,
        permission_policy_digest: authority.permission_policy_digest.clone(),
        approval_authority_id: authority.approval_authority_id.clone(),
        expires_at_ms: authority.expires_at_ms,
        mutation_batch_id: batch_id.to_owned(),
    };
    append_unique_operation_event(
        store,
        DurableEventType::IntentOperationPrepared,
        event,
        &resolved.preview.operation_id,
    )
}

fn append_operation_resolved(
    store: &JsonlSessionStore,
    operation_id: &IntentOperationId,
    resolution: IntentOperationResolution,
    batch_id: Option<&str>,
    result_snapshot_id: Option<&str>,
    error_code: Option<IntentOperationErrorCode>,
) -> Result<()> {
    let event = IntentEventV1::OperationResolved {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        resolution,
        mutation_batch_id: batch_id.map(str::to_owned),
        result_snapshot_id: result_snapshot_id.map(str::to_owned),
        error_code,
    };
    append_unique_operation_event(
        store,
        DurableEventType::IntentOperationResolved,
        event,
        operation_id,
    )
}

fn append_conflicted_operation(
    store: &JsonlSessionStore,
    preview: &IntentOperationPreviewV1,
) -> Result<()> {
    let mut events = preview
        .conflicts
        .iter()
        .cloned()
        .map(|conflict| {
            let event = IntentEventV1::ConflictRecorded {
                schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
                stack_id: preview.stack_id.clone(),
                stack_version: preview.stack_version,
                conflict,
            };
            Ok((
                DurableEventType::IntentConflictRecorded,
                EventClass::Critical,
                serde_json::to_value(event)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let resolved = IntentEventV1::OperationResolved {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        operation_id: preview.operation_id.clone(),
        resolution: IntentOperationResolution::Conflicted,
        mutation_batch_id: None,
        result_snapshot_id: None,
        error_code: Some(IntentOperationErrorCode::IntentStateConflict),
    };
    events.push((
        DurableEventType::IntentOperationResolved,
        EventClass::Critical,
        serde_json::to_value(resolved)?,
    ));
    let operation_id = preview.operation_id.clone();
    store.append_events_and_session_entries_if(events, &[], move |records| {
        Ok(!records.iter().any(|record| {
            operation_event_id(record.stored_event())
                .as_ref()
                .is_some_and(|id| id == &operation_id)
                && record.stored_event().event_kind()
                    == Some(DurableEventType::IntentOperationResolved)
        }))
    })?;
    Ok(())
}

fn append_unique_operation_event(
    store: &JsonlSessionStore,
    event_type: DurableEventType,
    event: IntentEventV1,
    operation_id: &IntentOperationId,
) -> Result<()> {
    event.validate_contract()?;
    let expected = event.clone();
    let operation_id = operation_id.clone();
    store.append_events_and_session_entries_if(
        vec![(
            event_type,
            EventClass::Critical,
            serde_json::to_value(event)?,
        )],
        &[],
        move |records| {
            for record in records {
                let stored = record.stored_event();
                if stored.event_kind() != Some(event_type) {
                    continue;
                }
                if operation_event_id(stored).as_ref() != Some(&operation_id) {
                    continue;
                }
                let existing = decode_operation_event(stored.clone())?;
                if existing != expected {
                    bail!("Intent operation event identity is already bound to another payload");
                }
                return Ok(false);
            }
            Ok(true)
        },
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finish_failed_drop(
    store: &JsonlSessionStore,
    recorder: &MutationEventRecorder,
    preview: &IntentOperationPreviewV1,
    authority: &IntentOperationAuthorityV1,
    batch_id: &str,
    committed_operation_ids: &[String],
    failed_operation_ids: &[String],
    result_snapshot_id: Option<String>,
    _source_error: anyhow::Error,
) -> Result<IntentOperationExecutionV1> {
    let status = if committed_operation_ids.is_empty() {
        MutationBatchStatus::Failed
    } else {
        MutationBatchStatus::PartiallyApplied
    };
    recorder.append_bound_batch_finished(
        batch_id,
        status,
        committed_operation_ids,
        failed_operation_ids,
        &[],
        &[],
        Some(preview.preview_digest.as_str()),
        Some(&authority.approval_authority_id),
        Some(authority.permission_policy_digest.as_str()),
    )?;
    let (resolution, error_code) = if committed_operation_ids.is_empty() {
        (
            IntentOperationResolution::Interrupted,
            IntentOperationErrorCode::ReconciliationRequired,
        )
    } else {
        (
            IntentOperationResolution::PartiallyApplied,
            IntentOperationErrorCode::PartialApplication,
        )
    };
    append_operation_resolved(
        store,
        &preview.operation_id,
        resolution,
        Some(batch_id),
        result_snapshot_id.as_deref(),
        Some(error_code),
    )?;
    Ok(IntentOperationExecutionV1 {
        preview: preview.clone(),
        resolution,
        mutation_batch_id: Some(batch_id.to_owned()),
        committed_operation_ids: committed_operation_ids.to_vec(),
        result_snapshot_id,
        error_code: Some(error_code),
    })
}

fn execution_from_existing(
    operation: &OperationRecord,
    resolved: &ResolvedOperation,
    batches: &BatchEvidence,
) -> Result<IntentOperationExecutionV1> {
    let committed_operation_ids = operation
        .prepared
        .as_ref()
        .map(|prepared| batches.outcome(operation, prepared))
        .transpose()?
        .map(|outcome| match outcome {
            BatchOutcome::Applied { operation_ids, .. } => operation_ids,
            BatchOutcome::FullyAppliedUnfinished { operation_ids, .. } => operation_ids,
            BatchOutcome::Partial { applied, .. } | BatchOutcome::Conflicted { applied, .. } => {
                applied
            }
            BatchOutcome::NoBatch | BatchOutcome::Interrupted { .. } => Vec::new(),
        })
        .unwrap_or_default();
    Ok(IntentOperationExecutionV1 {
        preview: operation.preview.clone(),
        resolution: resolved.resolution,
        mutation_batch_id: resolved.mutation_batch_id.clone(),
        committed_operation_ids,
        result_snapshot_id: resolved.result_snapshot_id.clone(),
        error_code: resolved.error_code,
    })
}

fn decode_operation_event(event: crate::StoredEvent) -> Result<IntentEventV1> {
    match decode_typed_stored_event(event)? {
        TypedStoredEventDecode::Known(event) => match *event {
            TypedDomainEvent::Intent(intent_event) => Ok(intent_event),
            _ => bail!("R51.4 wire type did not decode as an Intent event"),
        },
        TypedStoredEventDecode::UnknownNonCritical(_) => {
            bail!("R51.4 recovery-critical event decoded as unknown")
        }
    }
}

fn operation_event_id(event: &crate::StoredEvent) -> Option<IntentOperationId> {
    let value = serde_json::from_value::<IntentEventV1>(event.payload.clone()).ok()?;
    match value {
        IntentEventV1::OperationRequested { preview, .. } => Some(preview.operation_id),
        IntentEventV1::OperationPrepared { operation_id, .. }
        | IntentEventV1::OperationResolved { operation_id, .. } => Some(operation_id),
        _ => None,
    }
}

fn mutation_file_path(subject: &MutationSubject) -> Option<String> {
    let MutationSubject::File { path, .. } = subject else {
        return None;
    };
    path.to_str().map(str::to_owned)
}

fn intent_conflict(
    code: IntentOperationErrorCode,
    intent_ref: Option<IntentVersionRef>,
    artifact_id: Option<IntentArtifactId>,
    safe_reason: &str,
) -> IntentConflictV1 {
    IntentConflictV1 {
        code,
        intent_ref,
        artifact_id,
        safe_reason: safe_reason.to_owned(),
    }
}

fn intent_drop_batch_id(operation_id: &IntentOperationId) -> String {
    format!(
        "intent-drop-batch-{}",
        stable_event_uuid("sigil-intent-drop-batch", operation_id.as_str())
    )
}

fn zero_intent_digest() -> Result<IntentDigest> {
    IntentDigest::new(format!(
        "{}{}",
        INTENT_CANONICAL_DIGEST_PREFIX,
        "0".repeat(64)
    ))
}

fn validate_runtime_identity(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("{label} is empty, too long, or contains control characters");
    }
    Ok(())
}

fn now_ms() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before Unix epoch")?
            .as_millis(),
    )
    .unwrap_or(u64::MAX))
}

pub(crate) fn intent_operation_mutation_operation_ids(
    records: &[SessionStreamRecord],
) -> Result<BTreeSet<String>> {
    let mut batch_ids = BTreeSet::new();
    for record in records {
        let event = record.stored_event();
        if event.event_kind() != Some(DurableEventType::IntentOperationPrepared) {
            continue;
        }
        let prepared = decode_operation_event(event.clone())
            .context("failed to decode Intent checkpoint exclusion authority")?;
        let IntentEventV1::OperationPrepared {
            mutation_batch_id, ..
        } = prepared
        else {
            bail!("Intent operation prepared wire carried another payload");
        };
        batch_ids.insert(mutation_batch_id);
    }
    let mut operation_ids = BTreeSet::new();
    for record in records {
        let event = record.stored_event();
        if event.event_kind() != Some(DurableEventType::MutationPrepared) {
            continue;
        }
        let prepared: MutationPrepared = serde_json::from_value(event.payload.clone())
            .context("failed to decode Intent checkpoint exclusion prepare")?;
        if prepared
            .batch_id
            .as_ref()
            .is_some_and(|batch_id| batch_ids.contains(batch_id))
        {
            operation_ids.insert(prepared.operation_id);
        }
    }
    Ok(operation_ids)
}

pub(crate) fn checkpoint_intent_conflict_paths(
    records: &[SessionStreamRecord],
    checkpoint_boundary_sequence: u64,
) -> Result<BTreeSet<String>> {
    let admission = IntentStackProjectionV1::from_records(records)?;
    if admission.latest_accepted_plan().is_none() {
        return Ok(BTreeSet::new());
    }
    let lineage = crate::IntentLineageProjectionV1::from_records(records, &admission)?;
    let layers = IntentLayerProjectionV1::from_records(records, &admission, &lineage)?;
    let replay = OperationReplay::from_records(records, &admission, &layers)?;
    let operations = replay.public_projection();
    let mut paths = BTreeSet::new();
    for layer in layers.layers.values() {
        if operations.is_dropped(&layer.layer_manifest.core.intent_ref) {
            continue;
        }
        paths.extend(layer.artifacts.iter().filter_map(|artifact| {
            let BoundedIntentArtifactSubjectV1::FileHunk {
                normalized_relative_path,
                ..
            } = &artifact.binding.subject
            else {
                return None;
            };
            Some(normalized_relative_path.clone())
        }));
    }
    for operation in replay.operations.values().filter(|operation| {
        operation.requested_sequence > checkpoint_boundary_sequence
            || operation
                .prepared_sequence
                .is_some_and(|sequence| sequence > checkpoint_boundary_sequence)
            || operation
                .resolved_sequence
                .is_some_and(|sequence| sequence > checkpoint_boundary_sequence)
    }) {
        paths.extend(
            operation
                .preview
                .file_effects
                .iter()
                .map(|file| file.normalized_relative_path.clone()),
        );
    }
    Ok(paths)
}

#[cfg(test)]
#[path = "tests/intent_operation_tests.rs"]
mod tests;
