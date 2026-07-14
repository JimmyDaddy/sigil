use anyhow::{Context, Result, bail};

use super::compaction_v2::{compaction_lifecycle_event_id, compaction_session_id};
use super::writer::PendingStoredEvent;
use super::*;
use crate::{
    EventId, FrozenProviderRequestMaterial, ProviderNonGeneratingAttemptReceipt, RequestFitProof,
    TokenMeasurementBinding,
};

/// Input for one portable semantic-compaction attempt.
///
/// This contract deliberately accepts an already parsed strict compressor response. It performs
/// no provider I/O. Callers must separately establish the exact frozen target request and its
/// request-fit proof before executing the attempt.
#[derive(Debug, Clone)]
pub struct PortableSemanticCompactionRequest {
    /// Stable identity for this one append-only lifecycle attempt.
    pub attempt_id: CompactionAttemptId,
    /// Stable identity for the activated V2 compaction boundary on success.
    pub compaction_id: CompactionId,
    /// Explicit source that admitted this initiated lifecycle.
    pub initiation: CompactionInitiation,
    /// Versioned revision of the deterministic source projection used for this attempt.
    pub base_projection_revision: String,
    /// Optional branch binding for the resulting checkpoint.
    pub branch_id: Option<crate::BranchId>,
    /// Workspace snapshot that bounds validity of the resulting checkpoint.
    pub valid_for_snapshot: crate::WorkspaceSnapshotId,
    /// Optional user objective retained by the checkpoint.
    pub objective: Option<String>,
    /// Language used for the rendered checkpoint framing.
    pub language: String,
    /// Exact safe fold plan reconstructed from the current V2 stream.
    pub plan: CompactionFoldPlan,
    /// Strict, already-parsed semantic continuation material.
    pub model_output: ContinuationModelOutputV1,
    /// Bounded policy for optional historical tool-output projection shrink.
    pub tool_output_projection_policy: ToolOutputProjectionPolicy,
    /// Wall-clock timestamp recorded by the durable Started lifecycle entry.
    pub started_at_unix_ms: u64,
    /// Wall-clock timestamp recorded by the durable Applied lifecycle entry.
    pub completed_at_unix_ms: u64,
}

/// Durable result of one portable semantic-compaction attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableSemanticCompactionOutcome {
    /// Activated V2 compaction identity.
    pub compaction_id: CompactionId,
    /// Lifecycle attempt identity that reached Applied.
    pub attempt_id: CompactionAttemptId,
    /// Durable task-memory sidecar identity recorded with the activation.
    pub task_memory_id: crate::TaskMemoryId,
    /// Whether the optional tool-output projection sidecar was recorded.
    pub tool_output_projection_recorded: bool,
}

/// Process-local preflight for one portable semantic compaction apply.
///
/// It carries user-derived checkpoint material only in memory. The preflight is valid only for
/// the exact durable source cursor it was built from; no lifecycle record is appended until the
/// caller has frozen and admitted the full post-activation request.
#[derive(Debug)]
pub struct PortableSemanticCompactionPreflight {
    request: PortableSemanticCompactionRequest,
    session_id: crate::SessionId,
    source_record_count: usize,
    interleaved_input_token_measurement: Option<ProviderNonGeneratingAttemptReceipt>,
    prepared: PreparedPortableCheckpoint,
    candidate_messages: Vec<crate::ModelMessage>,
}

impl PortableSemanticCompactionPreflight {
    /// Returns the unpersisted continuation checkpoint for the exact preflight source stream.
    #[must_use]
    pub fn checkpoint(&self) -> &ContinuationCheckpointV1 {
        &self.prepared.applied.checkpoint
    }

    /// Returns the unpersisted task-memory sidecar for the exact preflight source stream.
    #[must_use]
    pub fn task_memory(&self) -> &TaskMemoryV1 {
        &self.prepared.task_memory_record.memory
    }

    /// Returns the complete provider-visible candidate: checkpoint followed by retained raw tail.
    #[must_use]
    pub fn candidate_messages(&self) -> &[crate::ModelMessage] {
        &self.candidate_messages
    }

    /// Binds one completed non-generating token measurement that occurred after this preflight.
    ///
    /// The executor permits exactly this start/terminal pair between the planned source and its
    /// portable `Started` barrier. The pair must be for the same session and frozen target
    /// fingerprint; any other durable append remains a stale-plan failure.
    ///
    /// # Errors
    ///
    /// Returns an error when the receipt is not an input-token measurement, belongs to another
    /// session or frozen target, or a measurement has already been bound.
    pub fn admit_completed_input_token_measurement(
        &mut self,
        receipt: ProviderNonGeneratingAttemptReceipt,
        expected_request_material_fingerprint: &str,
    ) -> Result<()> {
        if receipt.purpose() != ProviderPhysicalAttemptPurpose::InputTokenMeasurement {
            bail!("portable preflight only admits input-token measurement receipts");
        }
        if receipt.session_scope_id() != self.session_id {
            bail!("input-token measurement belongs to a different session scope");
        }
        if receipt.request_material_fingerprint() != expected_request_material_fingerprint {
            bail!("input-token measurement does not bind the portable frozen target");
        }
        if self.interleaved_input_token_measurement.is_some() {
            bail!("portable preflight already has an interleaved input-token measurement");
        }
        self.interleaved_input_token_measurement = Some(receipt);
        Ok(())
    }

    fn validate_source_records(&self, records: &[SessionStreamRecord]) -> Result<()> {
        if records.len() < self.source_record_count {
            bail!("portable semantic compaction source stream changed before Start");
        }
        let (source_records, interleaved_records) = records.split_at(self.source_record_count);
        let source_tail = source_records
            .last()
            .context("portable semantic compaction source stream is empty")?;
        if source_tail.projection_cursor(COMPACTION_FOLD_PLAN_SCHEMA_VERSION)
            != self.request.plan.base_stream_cursor
        {
            bail!("portable semantic compaction source cursor changed before Start");
        }
        validate_request_against_source_records(source_records, &self.request)?;
        match &self.interleaved_input_token_measurement {
            Some(receipt) => {
                validate_interleaved_input_token_measurement(records, interleaved_records, receipt)
            }
            None if interleaved_records.is_empty() => Ok(()),
            None => bail!("portable semantic compaction source stream changed before Start"),
        }
    }

    fn attach_and_validate_target_material(
        &mut self,
        target_material: PortableTargetRequestMaterial,
    ) -> Result<()> {
        let PortableTargetRequestMaterial {
            frozen_request,
            binding,
            proof,
        } = target_material;
        let target_request_fit = ContinuationTargetRequestFitV1 {
            material_fingerprint: frozen_request.fingerprint().to_owned(),
            binding,
            proof,
        };
        self.prepared
            .applied
            .checkpoint
            .attach_target_request_fit(target_request_fit)
            .and_then(|()| {
                self.prepared
                    .applied
                    .checkpoint
                    .validate_for_frozen_target_request(
                        &self.prepared.task_memory_record.memory,
                        &self.session_id,
                        &frozen_request,
                    )
            })?;
        validate_frozen_target_contains_candidate(&frozen_request, &self.candidate_messages)
    }
}

/// Process-local material and proof for the actual next request that will carry a portable
/// checkpoint.
///
/// The frozen bytes never enter the durable event stream. Only their session-bound fingerprint
/// and token-proof binding are persisted in `ContinuationTargetRequestFitV1`.
#[derive(Debug)]
pub struct PortableTargetRequestMaterial {
    pub(crate) frozen_request: FrozenProviderRequestMaterial,
    pub(crate) binding: TokenMeasurementBinding,
    pub(crate) proof: RequestFitProof,
}

impl PortableTargetRequestMaterial {
    /// Binds one frozen provider request to its exact token-measurement profile and fit proof.
    #[must_use]
    pub fn new(
        frozen_request: FrozenProviderRequestMaterial,
        binding: TokenMeasurementBinding,
        proof: RequestFitProof,
    ) -> Self {
        Self {
            frozen_request,
            binding,
            proof,
        }
    }

    /// Returns the fit proof that will be persisted with the activated checkpoint.
    #[must_use]
    pub fn proof(&self) -> &RequestFitProof {
        &self.proof
    }

    /// Returns the process-local frozen target that is bound to this proof.
    ///
    /// The caller may retain a clone only to hand the exact same first request to the immediately
    /// following provider turn after durable activation. It must not persist or render the raw
    /// request material.
    #[must_use]
    pub fn frozen_request(&self) -> &FrozenProviderRequestMaterial {
        &self.frozen_request
    }
}

impl JsonlSessionStore {
    /// Builds the process-local post-activation checkpoint and retained transcript projection.
    ///
    /// This performs no durable write and no provider I/O. Callers must use the returned
    /// candidate to build and freeze the actual target request before invoking
    /// [`JsonlSessionStore::execute_portable_semantic_compaction`].
    ///
    /// # Errors
    ///
    /// Returns an error when the source stream is malformed, no safe fold can be built, or the
    /// requested plan no longer matches the current durable session state.
    pub fn prepare_portable_semantic_compaction(
        &self,
        request: PortableSemanticCompactionRequest,
    ) -> Result<PortableSemanticCompactionPreflight> {
        validate_request_shape(&request)?;
        let source_records = self.read_event_records_writer()?;
        validate_request_against_source_records(&source_records, &request)?;
        let session_id = compaction_session_id_from_records(&source_records)?;
        let prepared = prepare_portable_checkpoint(&source_records, &request)?;
        let candidate_messages =
            crate::session::context_projection::portable_candidate_model_messages(
                &source_records,
                &prepared.applied.folded_through,
                &prepared.applied.checkpoint,
                &prepared.task_memory_record.memory,
            )?;
        Ok(PortableSemanticCompactionPreflight {
            request,
            session_id,
            source_record_count: source_records.len(),
            interleaved_input_token_measurement: None,
            prepared,
            candidate_messages,
        })
    }

    /// Executes a preflighted portable semantic compaction after its exact target has been
    /// frozen and proven.
    ///
    /// The material proof is checked before `Started`. The durable writer then compares the
    /// preflight source cursor and appends only `Started`, the typed TaskMemory/Applied batch, and
    /// an optional tool-output sidecar. Raw JSONL messages are never rewritten or deleted.
    ///
    /// # Errors
    ///
    /// Returns an error when the source changed, the frozen request/proof is not an exact match,
    /// or the append-only lifecycle transition cannot be durably committed.
    pub fn execute_portable_semantic_compaction(
        &self,
        mut preflight: PortableSemanticCompactionPreflight,
        target_material: PortableTargetRequestMaterial,
    ) -> Result<PortableSemanticCompactionOutcome> {
        preflight
            .attach_and_validate_target_material(target_material)
            .context("portable semantic target request proof is invalid")?;

        let started = self.append_preflight_compaction_started(&preflight)?;

        if let Err(error) = self.append_portable_completion_batch(
            &preflight,
            &started,
            &preflight.prepared.task_memory_record,
            &preflight.prepared.applied,
        ) {
            append_failure_if_open(
                self,
                &preflight.request.attempt_id,
                CompactionFailureReason::ExecutionFailed,
                preflight.request.completed_at_unix_ms,
            )?;
            return Err(error.context("portable semantic compaction completion append failed"));
        }

        let tool_output_projection_recorded =
            if let Some(sidecar) = preflight.prepared.tool_output_sidecar {
                self.append_tool_output_projection_shrink_recorded(sidecar)
                    .is_ok()
            } else {
                false
            };

        Ok(PortableSemanticCompactionOutcome {
            compaction_id: preflight.request.compaction_id,
            attempt_id: preflight.request.attempt_id,
            task_memory_id: preflight.prepared.task_memory_record.memory.memory_id,
            tool_output_projection_recorded,
        })
    }

    fn append_preflight_compaction_started(
        &self,
        preflight: &PortableSemanticCompactionPreflight,
    ) -> Result<StoredEvent> {
        let entry = CompactionStartedEntry {
            attempt_id: preflight.request.attempt_id.clone(),
            fallback_parent: CompactionFallbackParent::Root,
            initiation: preflight.request.initiation.clone(),
            base_projection_revision: preflight.request.base_projection_revision.clone(),
            started_at_unix_ms: preflight.request.started_at_unix_ms,
        };
        entry.validate_shape()?;
        let event_id =
            compaction_lifecycle_event_id(&preflight.session_id, &entry.attempt_id, "started");
        let payload = serde_json::to_value(&entry).context("failed to encode compaction start")?;
        self.append_event_if_with_identity(
            DurableEventType::CompactionStarted,
            payload,
            event_id.clone(),
            Some(event_id),
            None,
            |records| {
                preflight.validate_source_records(records)?;
                let projection = CompactionLifecycleProjection::from_records(records)?;
                projection.validate_started(&entry)?;
                Ok(true)
            },
        )?
        .context("portable semantic compaction Start was not appended")
    }

    fn append_portable_completion_batch(
        &self,
        preflight: &PortableSemanticCompactionPreflight,
        started: &StoredEvent,
        task_memory_record: &TaskMemoryRecordedV1,
        applied: &CompactionAppliedV2,
    ) -> Result<()> {
        let request = &preflight.request;
        let session_id = compaction_session_id(self)?;
        let started_event_id = started.event_id.clone();
        let memory_event_id = compaction_lifecycle_event_id(
            &session_id,
            &request.attempt_id,
            &format!("task-memory:{}", task_memory_record.memory.memory_id),
        );
        let applied_event_id =
            compaction_lifecycle_event_id(&session_id, &request.attempt_id, "applied");
        let memory_payload = serde_json::to_value(task_memory_record)
            .context("failed to encode portable task memory sidecar")?;
        let applied_payload = serde_json::to_value(applied)
            .context("failed to encode portable compaction applied")?;
        let pending = vec![
            PendingStoredEvent {
                event_type: DurableEventType::TaskMemoryRecordedV1,
                event_class: EventClass::Critical,
                payload: memory_payload,
                event_id: Some(memory_event_id.clone()),
                correlation_id: Some(started_event_id.clone()),
                causation_id: Some(started_event_id.clone()),
            },
            PendingStoredEvent {
                event_type: DurableEventType::CompactionAppliedV2,
                event_class: EventClass::Critical,
                payload: applied_payload,
                event_id: Some(applied_event_id),
                correlation_id: Some(started_event_id.clone()),
                causation_id: Some(started_event_id),
            },
        ];

        self.append_events_if_with_identities(pending, |records| {
            validate_completion_cas(records, preflight, started)?;
            let session_id = compaction_session_id_from_records(records)?;
            let memory = synthetic_event(
                DurableEventType::TaskMemoryRecordedV1,
                memory_event_id.clone(),
                session_id.clone(),
                next_stream_sequence(records),
                serde_json::to_value(task_memory_record)?,
                Some(started.event_id.clone()),
                Some(started.event_id.clone()),
            )?;
            let mut virtual_records = records.to_vec();
            virtual_records.push(SessionStreamRecord::Stored(memory));
            let applied_event = synthetic_event(
                DurableEventType::CompactionAppliedV2,
                compaction_lifecycle_event_id(&session_id, &request.attempt_id, "applied"),
                session_id,
                next_stream_sequence(&virtual_records),
                serde_json::to_value(applied)?,
                Some(started.event_id.clone()),
                Some(started.event_id.clone()),
            )?;
            virtual_records.push(SessionStreamRecord::Stored(applied_event));
            CompactionLifecycleProjection::from_records(&virtual_records)?;
            CompactionSidecarProjection::from_records(&virtual_records)?;
            Ok(true)
        })?
        .context("portable semantic compaction completion was not appended")?;
        Ok(())
    }
}

fn validate_frozen_target_contains_candidate(
    frozen_request: &FrozenProviderRequestMaterial,
    candidate_messages: &[crate::ModelMessage],
) -> Result<()> {
    if candidate_messages.is_empty() {
        bail!("portable semantic candidate must include its rendered checkpoint");
    }
    let rendered_candidate = candidate_messages
        .iter()
        .map(serde_json::to_value)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to encode portable semantic candidate messages")?;
    let rendered_request = frozen_request
        .request()
        .messages
        .iter()
        .map(serde_json::to_value)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to encode frozen target request messages")?;
    let occurrences = rendered_request
        .windows(rendered_candidate.len())
        .filter(|window| *window == rendered_candidate.as_slice())
        .count();
    if occurrences != 1 {
        bail!(
            "frozen target request must contain the portable checkpoint and retained projection exactly once"
        );
    }
    Ok(())
}

#[derive(Debug)]
struct PreparedPortableCheckpoint {
    task_memory_record: TaskMemoryRecordedV1,
    applied: CompactionAppliedV2,
    tool_output_sidecar: Option<ToolOutputProjectionShrinkRecorded>,
}

fn validate_request_shape(request: &PortableSemanticCompactionRequest) -> Result<()> {
    if request.attempt_id.trim().is_empty()
        || request.compaction_id.trim().is_empty()
        || request.base_projection_revision.trim().is_empty()
        || request.valid_for_snapshot.trim().is_empty()
        || request.language.trim().is_empty()
        || request
            .branch_id
            .as_deref()
            .is_some_and(|branch_id| branch_id.trim().is_empty())
    {
        bail!("portable semantic compaction request identity is invalid");
    }
    if !request.plan.has_foldable_history() {
        bail!("portable semantic compaction request has no foldable history");
    }
    request.tool_output_projection_policy.validate()?;
    Ok(())
}

fn validate_request_against_source_records(
    records: &[SessionStreamRecord],
    request: &PortableSemanticCompactionRequest,
) -> Result<()> {
    request.plan.validate_against(records)?;
    let lifecycle = CompactionLifecycleProjection::from_records(records)?;
    if !lifecycle.unfinished_attempts().is_empty() {
        bail!("portable semantic compaction cannot start while another attempt is unfinished");
    }
    let sidecars = CompactionSidecarProjection::from_records(records)?;
    let active = sidecars.latest_for_branch(request.branch_id.as_deref());
    let expected_prior = active.map(|sidecar| &sidecar.folded_through);
    if request.plan.prior_folded_through.as_ref() != expected_prior {
        bail!("portable semantic compaction plan does not match the active compaction boundary");
    }
    Ok(())
}

fn prepare_portable_checkpoint(
    source_records: &[SessionStreamRecord],
    request: &PortableSemanticCompactionRequest,
) -> Result<PreparedPortableCheckpoint> {
    validate_request_against_source_records(source_records, request)?;
    let session_id = compaction_session_id_from_records(source_records)?;
    let sidecars = CompactionSidecarProjection::from_records(source_records)?;
    let active = sidecars.latest_for_branch(request.branch_id.as_deref());
    let memory_id = format!(
        "task-memory:{}",
        crate::stable_event_uuid(
            "sigil-portable-semantic-task-memory",
            &format!("{session_id}:{}", request.attempt_id),
        )
    );
    let task_memory = crate::extract_task_memory_from_stream_records(
        source_records,
        crate::TaskMemoryExtractionInput {
            memory_id,
            valid_for_snapshot: request.valid_for_snapshot.clone(),
            branch_id: request.branch_id.clone(),
            supersedes: active.map(|sidecar| sidecar.task_memory.memory_id.clone()),
            objective: request.objective.clone(),
        },
    )?;
    let folded_through = request
        .plan
        .folded_through
        .clone()
        .context("portable semantic compaction plan lost its fold boundary")?;
    let task_memory_record = TaskMemoryRecordedV1::new(folded_through.clone(), task_memory)?;
    let catalog = ContinuationSourceCatalog::from_fold_plan(source_records, &request.plan)?;
    let checkpoint = ContinuationCheckpointV1::from_catalog_and_model_output(
        request.language.clone(),
        &task_memory_record.memory,
        &catalog,
        &request.plan,
        request.model_output.clone(),
    )?;
    checkpoint.render_for_provider(&task_memory_record.memory)?;
    let tool_output_projection = ToolOutputProjection::from_fold_plan(
        source_records,
        &request.plan,
        &request.tool_output_projection_policy,
    )?;
    let tool_output_sidecar = (!tool_output_projection.outputs.is_empty())
        .then(|| {
            ToolOutputProjectionShrinkRecorded::from_projection(
                request.compaction_id.clone(),
                request.attempt_id.clone(),
                &request.plan,
                request.tool_output_projection_policy.clone(),
                &tool_output_projection,
            )
        })
        .transpose()?;
    Ok(PreparedPortableCheckpoint {
        applied: CompactionAppliedV2 {
            compaction_id: request.compaction_id.clone(),
            attempt_id: request.attempt_id.clone(),
            parent_compaction_id: active.map(|sidecar| sidecar.compaction_id.clone()),
            branch_id: request.branch_id.clone(),
            valid_for_snapshot: Some(request.valid_for_snapshot.clone()),
            task_memory_id: Some(task_memory_record.memory.memory_id.clone()),
            checkpoint,
            base_projection_revision: request.base_projection_revision.clone(),
            folded_through,
            applied_at_unix_ms: request.completed_at_unix_ms,
        },
        task_memory_record,
        tool_output_sidecar,
    })
}

fn validate_interleaved_input_token_measurement(
    all_records: &[SessionStreamRecord],
    interleaved_records: &[SessionStreamRecord],
    receipt: &ProviderNonGeneratingAttemptReceipt,
) -> Result<()> {
    if interleaved_records.len() != 2 {
        bail!("portable preflight input-token measurement records are incomplete or ambiguous");
    }
    let started_event = interleaved_records[0].stored_event();
    if started_event.event_kind() != Some(DurableEventType::ProviderPhysicalAttemptStarted)
        || started_event.session_id != receipt.session_scope_id()
    {
        bail!("portable preflight input-token measurement start does not match its receipt");
    }
    let started: ProviderPhysicalAttemptStartedEntry =
        serde_json::from_value(started_event.payload.clone())
            .context("portable preflight input-token measurement start payload is invalid")?;
    started.validate_shape()?;
    if started.physical_attempt_id != receipt.physical_attempt_id()
        || started.request_material_fingerprint != receipt.request_material_fingerprint()
        || started.purpose != ProviderPhysicalAttemptPurpose::InputTokenMeasurement
    {
        bail!("portable preflight input-token measurement start does not bind its receipt");
    }

    let terminal_event = interleaved_records[1].stored_event();
    if terminal_event.event_kind() != Some(DurableEventType::ProviderPhysicalAttemptTerminal)
        || terminal_event.session_id != receipt.session_scope_id()
    {
        bail!("portable preflight input-token measurement terminal does not match its receipt");
    }
    let terminal: ProviderPhysicalAttemptTerminalEntry =
        serde_json::from_value(terminal_event.payload.clone())
            .context("portable preflight input-token measurement terminal payload is invalid")?;
    terminal.validate_shape()?;
    if terminal.physical_attempt_id != receipt.physical_attempt_id()
        || terminal.request_material_fingerprint != receipt.request_material_fingerprint()
        || terminal.outcome != ProviderPhysicalAttemptOutcome::Completed
        || terminal.rejection.is_some()
        || !terminal.durable_output_event_ids.is_empty()
        || !terminal.durable_side_effect_event_ids.is_empty()
    {
        bail!(
            "portable preflight input-token measurement terminal is not a completed no-output measurement"
        );
    }
    ProviderPhysicalAttemptProjection::from_records(all_records)?;
    Ok(())
}

fn validate_completion_cas(
    records: &[SessionStreamRecord],
    preflight: &PortableSemanticCompactionPreflight,
    started: &StoredEvent,
) -> Result<()> {
    let expected_record_count = preflight.source_record_count
        + usize::from(preflight.interleaved_input_token_measurement.is_some()) * 2
        + 1;
    if records.len() != expected_record_count {
        bail!("portable semantic compaction source stream changed after its Start barrier");
    }
    let before_started = &records[..records.len().saturating_sub(1)];
    preflight.validate_source_records(before_started)?;
    let actual_started = records
        .last()
        .context("portable semantic compaction Start barrier is missing")?
        .stored_event();
    if actual_started.event_id != started.event_id
        || actual_started.event_kind() != Some(DurableEventType::CompactionStarted)
        || actual_started.correlation_id.as_deref() != Some(started.event_id.as_str())
    {
        bail!("portable semantic compaction Start barrier changed before completion");
    }
    let lifecycle = CompactionLifecycleProjection::from_records(records)?;
    let attempt = lifecycle
        .attempt(&preflight.request.attempt_id)
        .context("portable semantic compaction attempt is missing before completion")?;
    if attempt.terminal.is_some() {
        bail!("portable semantic compaction attempt is already terminal");
    }
    Ok(())
}

fn append_failure_if_open(
    store: &JsonlSessionStore,
    attempt_id: &str,
    reason: CompactionFailureReason,
    failed_at_unix_ms: u64,
) -> Result<()> {
    let records = store.read_event_records_writer()?;
    let lifecycle = CompactionLifecycleProjection::from_records(&records)?;
    if lifecycle
        .attempt(attempt_id)
        .is_some_and(|attempt| attempt.terminal.is_none())
    {
        store.append_compaction_failed(CompactionFailureEntry {
            attempt_id: attempt_id.to_owned(),
            reason,
            failed_at_unix_ms,
        })?;
    }
    Ok(())
}

fn compaction_session_id_from_records(records: &[SessionStreamRecord]) -> Result<crate::SessionId> {
    records
        .first()
        .map(|record| record.session_id().to_owned())
        .context("portable semantic compaction requires a non-empty durable stream")
}

fn synthetic_event(
    event_type: DurableEventType,
    event_id: EventId,
    session_id: crate::SessionId,
    stream_sequence: u64,
    payload: serde_json::Value,
    correlation_id: Option<EventId>,
    causation_id: Option<EventId>,
) -> Result<StoredEvent> {
    let mut event = StoredEvent::new(
        event_type,
        event_type
            .expected_event_class()
            .context("portable semantic event has no registered class")?,
        event_id,
        session_id,
        stream_sequence,
        payload,
    )?;
    event.correlation_id = correlation_id;
    event.causation_id = causation_id;
    event.record_checksum = event.compute_record_checksum()?;
    Ok(event)
}

#[cfg(test)]
#[path = "tests/portable_compaction_tests.rs"]
mod tests;
