use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::event::canonical_json_bytes;
use crate::{
    BoundedIntentArtifactSubjectV1, ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetId,
    ChangeSetResult, ChangeSetResultStatus, ControlEntry, ConversationRunLifecycleRecordV1,
    ConversationRunTerminalStatusV1, DurableEventType, EventClass, FileType,
    INTENT_CONTRACT_SCHEMA_VERSION, IntegrationPromotionEffect, IntegrationPromotionRecorded,
    IntegrationPromotionStatus, IntegrationPromotionTarget, IntentApplicationState,
    IntentArtifactAvailability, IntentArtifactBindingV1, IntentArtifactId, IntentArtifactKind,
    IntentArtifactManifestV1, IntentArtifactOwnership, IntentArtifactProvenanceV1,
    IntentByteRangeV1, IntentContentDigest, IntentEventV1, IntentExecutionId,
    IntentExecutionLineageV1, IntentExecutionOriginV1, IntentLayerCoreV1, IntentLayerManifestV1,
    IntentStackProjectionV1, IntentVersionRef, JsonlSessionStore,
    MAX_WORKSPACE_SNAPSHOT_FILE_BYTES, MutationArtifactLifecycleRecorded,
    MutationArtifactLifecycleStatus, MutationBatchFinished, MutationBatchStatus, MutationCommitted,
    MutationEventRecorder, MutationPrepared, MutationSubject, PublicIntentArtifactSummaryV1,
    SecretRedactor, Session, SessionLogEntry, SessionStreamRecord, SnapshotCoverage,
    TaskParticipantAttemptEntry, TaskParticipantAttemptStatus, TypedDomainEvent,
    TypedStoredEventDecode, WorkspaceMutationDetected,
    conversation_run_lifecycle_record_from_stream, decode_typed_stored_event,
    is_sensitive_mutation_artifact_path, stable_workspace_id,
};

/// Projection schema for R51.3 layer and artifact materialization.
pub const INTENT_LAYER_PROJECTION_SCHEMA_VERSION: u16 = 1;

const INTENT_PATCH_MAGIC: &[u8] = b"sigil-intent-patch-v1\0";
const INTENT_HUNK_MAGIC: &[u8] = b"sigil-intent-hunk-v1\0";
const MAX_INTENT_PATCH_FILES: usize = 1_024;

/// Why an accepted execution cannot become an executable layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentLayerReadOnlyReasonV1 {
    MissingTerminalAttempt,
    MissingMutationArtifact,
    UnsupportedFileKind,
    SensitiveContent,
    ArtifactUnavailable,
    SharedOwnership,
    UnownedArtifact,
    DriftedArtifact,
}

/// Runtime materialization result. Read-only outcomes never append layer authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentLayerMaterializationOutcomeV1 {
    pub appended: bool,
    pub execution_id: IntentExecutionId,
    pub manifest_digest: Option<String>,
    pub read_only_reason: Option<IntentLayerReadOnlyReasonV1>,
}

/// Effective artifact state after lifecycle, overlap and drift reduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveIntentArtifactV1 {
    pub binding: IntentArtifactBindingV1,
    pub ownership: IntentArtifactOwnership,
    pub availability: IntentArtifactAvailability,
}

/// One complete artifact-first/final-manifest layer pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentLayerStateV1 {
    pub artifact_manifest: IntentArtifactManifestV1,
    pub layer_manifest: IntentLayerManifestV1,
    pub artifact_event_id: String,
    pub layer_event_id: String,
    pub layer_stream_sequence: u64,
    pub artifacts: Vec<EffectiveIntentArtifactV1>,
}

/// Bounded layer summary consumed by the public Intent Stack adapter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntentLayerSummaryV1 {
    pub application_state: Option<IntentApplicationState>,
    pub exclusive_artifact_count: u32,
    pub shared_artifact_count: u32,
    pub unowned_artifact_count: u32,
    pub drifted_artifact_count: u32,
    pub unavailable_artifact_count: u32,
    pub artifacts: Vec<PublicIntentArtifactSummaryV1>,
    pub read_only_reason: Option<IntentLayerReadOnlyReasonV1>,
}

/// Append-only R51.3 layer projection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntentLayerProjectionV1 {
    pub layers: BTreeMap<IntentExecutionId, IntentLayerStateV1>,
    pub layer_order: Vec<IntentExecutionId>,
}

impl IntentLayerProjectionV1 {
    /// Replays complete artifact-manifest/layer-manifest pairs and derives conservative ownership.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed registered events, incomplete ordering, forged lineage or
    /// conflicting layer identity.
    pub fn from_records(
        records: &[SessionStreamRecord],
        admission: &IntentStackProjectionV1,
        lineage: &crate::IntentLineageProjectionV1,
    ) -> Result<Self> {
        let facts = LayerMaterializationFacts::from_records(records)?;
        let mut pending =
            BTreeMap::<IntentExecutionId, (IntentArtifactManifestV1, String, u64)>::new();
        let mut projection = Self::default();
        for record in records {
            let event = record.stored_event();
            let Some(kind) = DurableEventType::from_event_type(&event.event_type) else {
                continue;
            };
            if !matches!(
                kind,
                DurableEventType::IntentArtifactBindingsRecorded
                    | DurableEventType::IntentLayerManifestRecorded
            ) {
                continue;
            }
            let intent_event = decode_registered_layer_event(event.clone())?;
            match intent_event {
                IntentEventV1::ArtifactBindingsRecorded { manifest, .. } => {
                    let execution = lineage
                        .execution(&manifest.execution_id)
                        .context("Intent artifact manifest has no execution lineage")?;
                    validate_artifact_manifest_lineage(&manifest, execution)?;
                    if pending
                        .insert(
                            manifest.execution_id.clone(),
                            (manifest, event.event_id.clone(), event.stream_sequence),
                        )
                        .is_some()
                    {
                        bail!("Intent execution repeats an artifact manifest");
                    }
                }
                IntentEventV1::LayerManifestRecorded { manifest, .. } => {
                    let execution_id = manifest.core.execution_id.clone();
                    let execution = lineage
                        .execution(&execution_id)
                        .context("Intent layer manifest has no execution lineage")?;
                    let (artifact_manifest, artifact_event_id, artifact_sequence) = pending
                        .remove(&execution_id)
                        .context("Intent layer manifest precedes its artifact manifest")?;
                    if artifact_sequence.saturating_add(1) != event.stream_sequence {
                        bail!("Intent layer manifest is not adjacent to its artifact manifest");
                    }
                    validate_complete_layer(
                        &manifest,
                        &artifact_manifest,
                        execution,
                        admission
                            .accepted_plan(execution.stack_version)
                            .context("Intent layer requires its accepted IntentPlan version")?,
                        &facts,
                        artifact_sequence,
                        event.stream_sequence,
                    )?;
                    if projection.layers.contains_key(&execution_id) {
                        bail!("Intent execution repeats a final layer manifest");
                    }
                    projection.layer_order.push(execution_id.clone());
                    projection.layers.insert(
                        execution_id,
                        IntentLayerStateV1 {
                            artifacts: artifact_manifest
                                .artifacts
                                .iter()
                                .cloned()
                                .map(|binding| EffectiveIntentArtifactV1 {
                                    ownership: binding.ownership,
                                    availability: binding.availability,
                                    binding,
                                })
                                .collect(),
                            artifact_manifest,
                            layer_manifest: manifest,
                            artifact_event_id,
                            layer_event_id: event.event_id.clone(),
                            layer_stream_sequence: event.stream_sequence,
                        },
                    );
                }
                _ => bail!("R51.3 event type carried another Intent payload"),
            }
        }
        if !pending.is_empty() {
            bail!("Intent artifact manifest is missing its adjacent final layer manifest");
        }
        projection.apply_artifact_lifecycle(records)?;
        projection.apply_workspace_drift(records)?;
        projection.apply_shared_file_ownership(admission.latest_accepted_plan());
        Ok(projection)
    }

    #[must_use]
    pub fn layer(&self, execution_id: &IntentExecutionId) -> Option<&IntentLayerStateV1> {
        self.layers.get(execution_id)
    }

    #[must_use]
    pub fn latest_layer_for(&self, intent_ref: &IntentVersionRef) -> Option<&IntentLayerStateV1> {
        self.layer_order.iter().rev().find_map(|execution_id| {
            self.layers
                .get(execution_id)
                .filter(|layer| &layer.layer_manifest.core.intent_ref == intent_ref)
        })
    }

    /// Verifies content-addressed blobs and degrades missing/corrupt layers without restoring
    /// authority from another source.
    ///
    /// # Errors
    ///
    /// Returns an error when the canonical artifact manifest cannot be re-encoded.
    pub fn refresh_artifact_availability(
        &mut self,
        recorder: &MutationEventRecorder,
    ) -> Result<()> {
        for layer in self.layers.values_mut() {
            let mut required = vec![
                layer.layer_manifest.core.forward_patch_artifact_id.clone(),
                layer.layer_manifest.core.reverse_patch_artifact_id.clone(),
                layer.layer_manifest.artifact_manifest_id.clone(),
            ];
            required.extend(
                layer
                    .artifact_manifest
                    .artifacts
                    .iter()
                    .map(|artifact| artifact.artifact_id.as_str().to_owned()),
            );
            let expected_manifest =
                canonical_json_bytes(&serde_json::to_value(&layer.artifact_manifest)?)?;
            let manifest_matches = recorder
                .read_immutable_content_artifact(&layer.layer_manifest.artifact_manifest_id)
                .is_ok_and(|bytes| bytes == expected_manifest);
            if !manifest_matches
                || required
                    .iter()
                    .any(|id| recorder.read_immutable_content_artifact(id).is_err())
            {
                for artifact in &mut layer.artifacts {
                    if artifact.availability == IntentArtifactAvailability::Available {
                        artifact.availability = IntentArtifactAvailability::Corrupted;
                    }
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn summary_for(&self, intent_ref: &IntentVersionRef) -> IntentLayerSummaryV1 {
        let Some(layer) = self.latest_layer_for(intent_ref) else {
            return IntentLayerSummaryV1::default();
        };
        let mut summary = IntentLayerSummaryV1 {
            application_state: Some(IntentApplicationState::Applied),
            ..IntentLayerSummaryV1::default()
        };
        for artifact in &layer.artifacts {
            match artifact.ownership {
                IntentArtifactOwnership::Exclusive => {
                    summary.exclusive_artifact_count =
                        summary.exclusive_artifact_count.saturating_add(1);
                }
                IntentArtifactOwnership::Shared => {
                    summary.shared_artifact_count = summary.shared_artifact_count.saturating_add(1);
                    summary.read_only_reason = Some(IntentLayerReadOnlyReasonV1::SharedOwnership);
                }
                IntentArtifactOwnership::Unowned => {
                    summary.unowned_artifact_count =
                        summary.unowned_artifact_count.saturating_add(1);
                    summary.read_only_reason = Some(IntentLayerReadOnlyReasonV1::UnownedArtifact);
                }
                IntentArtifactOwnership::Drifted => {
                    summary.drifted_artifact_count =
                        summary.drifted_artifact_count.saturating_add(1);
                    summary.read_only_reason = Some(IntentLayerReadOnlyReasonV1::DriftedArtifact);
                }
            }
            if artifact.availability != IntentArtifactAvailability::Available {
                summary.unavailable_artifact_count =
                    summary.unavailable_artifact_count.saturating_add(1);
                summary.read_only_reason = Some(IntentLayerReadOnlyReasonV1::ArtifactUnavailable);
            }
            let normalized_relative_path = match &artifact.binding.subject {
                BoundedIntentArtifactSubjectV1::FileHunk {
                    normalized_relative_path,
                    ..
                } => Some(normalized_relative_path.clone()),
                _ => None,
            };
            summary.artifacts.push(PublicIntentArtifactSummaryV1 {
                artifact_id: artifact.binding.artifact_id.clone(),
                artifact_kind: artifact.binding.artifact_kind,
                ownership: artifact.ownership,
                availability: artifact.availability,
                normalized_relative_path,
            });
        }
        if summary.read_only_reason.is_some() {
            summary.application_state = Some(IntentApplicationState::ReadOnly);
        }
        summary
    }

    fn apply_artifact_lifecycle(&mut self, records: &[SessionStreamRecord]) -> Result<()> {
        let mut lifecycle = BTreeMap::<String, (u64, IntentArtifactAvailability)>::new();
        for record in records {
            let event = record.stored_event();
            if event.event_kind() != Some(DurableEventType::MutationArtifactLifecycleRecorded) {
                continue;
            }
            let value: MutationArtifactLifecycleRecorded =
                serde_json::from_value(event.payload.clone())
                    .context("failed to decode Intent artifact lifecycle")?;
            let availability = match value.status {
                MutationArtifactLifecycleStatus::Deleted => IntentArtifactAvailability::Deleted,
                MutationArtifactLifecycleStatus::Expired => IntentArtifactAvailability::Expired,
                MutationArtifactLifecycleStatus::Unavailable => {
                    IntentArtifactAvailability::Corrupted
                }
            };
            lifecycle.insert(value.artifact_id, (event.stream_sequence, availability));
        }
        for layer in self.layers.values_mut() {
            let required_layer_artifacts = [
                layer.layer_manifest.core.forward_patch_artifact_id.as_str(),
                layer.layer_manifest.core.reverse_patch_artifact_id.as_str(),
                layer.layer_manifest.artifact_manifest_id.as_str(),
            ];
            let layer_availability = required_layer_artifacts
                .iter()
                .filter_map(|id| lifecycle.get(*id).copied())
                .filter(|(sequence, _)| *sequence > layer.layer_stream_sequence)
                .map(|(_, availability)| availability)
                .next();
            for artifact in &mut layer.artifacts {
                artifact.availability = lifecycle
                    .get(artifact.binding.artifact_id.as_str())
                    .copied()
                    .filter(|(sequence, _)| *sequence > layer.layer_stream_sequence)
                    .map(|(_, availability)| availability)
                    .or(layer_availability)
                    .unwrap_or(artifact.availability);
            }
        }
        Ok(())
    }

    fn apply_workspace_drift(&mut self, records: &[SessionStreamRecord]) -> Result<()> {
        let owned_mutation_events = self
            .layers
            .values()
            .flat_map(|layer| layer.artifacts.iter())
            .map(|artifact| artifact.binding.provenance.source_event_id.clone())
            .collect::<BTreeSet<_>>();
        for layer in self.layers.values_mut() {
            for record in records
                .iter()
                .filter(|record| record.stream_sequence() > layer.layer_stream_sequence)
            {
                let event = record.stored_event();
                match event.event_kind() {
                    Some(DurableEventType::MutationCommitted) => {
                        if owned_mutation_events.contains(event.event_id.as_str()) {
                            continue;
                        }
                        let committed: MutationCommitted =
                            serde_json::from_value(event.payload.clone())
                                .context("failed to decode post-layer mutation")?;
                        for artifact in &mut layer.artifacts {
                            let BoundedIntentArtifactSubjectV1::FileHunk {
                                normalized_relative_path,
                                after_file_digest,
                                ..
                            } = &artifact.binding.subject
                            else {
                                continue;
                            };
                            if mutation_subject_matches_path(
                                &committed.committed_subject,
                                normalized_relative_path,
                            ) && committed.observed_after_hash.as_deref()
                                != Some(after_file_digest.as_str())
                            {
                                artifact.ownership = IntentArtifactOwnership::Drifted;
                            }
                        }
                    }
                    Some(DurableEventType::WorkspaceMutationDetected) => {
                        let mutation: WorkspaceMutationDetected =
                            serde_json::from_value(event.payload.clone())
                                .context("failed to decode post-layer workspace mutation")?;
                        if mutation.unknown_dirty {
                            for artifact in &mut layer.artifacts {
                                if matches!(
                                    artifact.binding.subject,
                                    BoundedIntentArtifactSubjectV1::FileHunk { .. }
                                ) {
                                    artifact.ownership = IntentArtifactOwnership::Drifted;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn apply_shared_file_ownership(
        &mut self,
        accepted: Option<&crate::AcceptedIntentPlanProjectionV1>,
    ) {
        let Some(accepted) = accepted else {
            return;
        };
        let accepted_refs = accepted
            .plan
            .intents
            .iter()
            .map(|intent| intent.intent_ref.clone())
            .collect::<BTreeSet<_>>();
        let mut latest_by_intent = BTreeMap::<IntentVersionRef, IntentExecutionId>::new();
        for execution_id in &self.layer_order {
            if let Some(layer) = self.layers.get(execution_id)
                && accepted_refs.contains(&layer.layer_manifest.core.intent_ref)
            {
                latest_by_intent.insert(
                    layer.layer_manifest.core.intent_ref.clone(),
                    execution_id.clone(),
                );
            }
        }
        let mut owners = BTreeMap::<String, BTreeSet<IntentVersionRef>>::new();
        for (intent_ref, execution_id) in &latest_by_intent {
            if let Some(layer) = self.layers.get(execution_id) {
                for artifact in &layer.artifacts {
                    if let BoundedIntentArtifactSubjectV1::FileHunk {
                        normalized_relative_path,
                        ..
                    } = &artifact.binding.subject
                    {
                        owners
                            .entry(normalized_relative_path.clone())
                            .or_default()
                            .insert(intent_ref.clone());
                    }
                }
            }
        }
        for execution_id in latest_by_intent.values() {
            if let Some(layer) = self.layers.get_mut(execution_id) {
                for artifact in &mut layer.artifacts {
                    let BoundedIntentArtifactSubjectV1::FileHunk {
                        normalized_relative_path,
                        ..
                    } = &artifact.binding.subject
                    else {
                        continue;
                    };
                    if owners
                        .get(normalized_relative_path)
                        .is_some_and(|owners| owners.len() > 1)
                        && artifact.ownership != IntentArtifactOwnership::Drifted
                    {
                        artifact.ownership = IntentArtifactOwnership::Shared;
                    }
                }
            }
        }
    }
}

fn decode_registered_layer_event(event: crate::StoredEvent) -> Result<IntentEventV1> {
    match decode_typed_stored_event(event)? {
        TypedStoredEventDecode::Known(event) => match *event {
            TypedDomainEvent::Intent(intent_event) => Ok(intent_event),
            _ => bail!("R51.3 wire type did not decode as an Intent event"),
        },
        TypedStoredEventDecode::UnknownNonCritical(_) => {
            bail!("R51.3 recovery-critical event decoded as unknown")
        }
    }
}

fn validate_artifact_manifest_lineage(
    manifest: &IntentArtifactManifestV1,
    execution: &IntentExecutionLineageV1,
) -> Result<()> {
    manifest.validate_contract()?;
    if manifest.intent_ref != execution.binding.intent_ref
        || manifest.execution_id != execution.binding.execution_id
    {
        bail!("Intent artifact manifest does not match its execution");
    }
    Ok(())
}

fn validate_complete_layer(
    layer: &IntentLayerManifestV1,
    artifacts: &IntentArtifactManifestV1,
    execution: &IntentExecutionLineageV1,
    accepted: &crate::AcceptedIntentPlanProjectionV1,
    facts: &LayerMaterializationFacts,
    artifact_sequence: u64,
    layer_sequence: u64,
) -> Result<()> {
    layer.validate_contract()?;
    artifacts.validate_contract()?;
    if layer.core.intent_ref != execution.binding.intent_ref
        || layer.core.execution_id != execution.binding.execution_id
        || layer.core.execution_origin != execution.binding.origin
        || layer.core.result_snapshot_id.as_str()
            != execution
                .parent_snapshot_id
                .as_deref()
                .context("Intent layer execution has no parent snapshot")?
        || layer.core.changeset_ids
            != execution
                .changeset_ids
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>()
        || artifacts.intent_ref != layer.core.intent_ref
        || artifacts.execution_id != layer.core.execution_id
        || artifacts.layer_core_digest != layer.core.core_digest
        || layer.artifact_manifest_digest != artifacts.manifest_digest
        || !accepted
            .plan
            .intents
            .iter()
            .any(|intent| intent.intent_ref == layer.core.intent_ref)
    {
        bail!("Intent layer manifest does not match accepted execution lineage");
    }
    if facts.terminal_attempt_sequence(execution)? >= artifact_sequence {
        bail!("Intent layer precedes its exact terminal attempt");
    }
    if facts.base_snapshot_id(
        execution,
        &accepted.plan.workspace_id,
        Some(artifact_sequence),
    )? != layer.core.base_snapshot_id
    {
        bail!("Intent layer base snapshot does not match execution evidence");
    }
    if !layer.core.operation_ids.is_empty() {
        bail!("Initial Intent layer cannot contain operation lineage");
    }
    let mut expected_paths = BTreeSet::new();
    for change_set_id in &execution.changeset_ids {
        let change_set = facts
            .changesets
            .get(change_set_id)
            .context("Intent layer execution ChangeSet is unavailable")?;
        for file in &change_set.value.files {
            if file.action == ChangeSetFileAction::Rename
                || !expected_paths.insert(file.path.clone())
            {
                bail!("Intent layer execution has unsupported or overlapping file changes");
            }
        }
    }
    let mut artifact_paths = BTreeSet::new();
    for artifact in &artifacts.artifacts {
        if artifact.ownership != IntentArtifactOwnership::Exclusive
            || artifact.availability != IntentArtifactAvailability::Available
        {
            bail!("Initial Intent layer artifacts must be exclusive and available");
        }
        let BoundedIntentArtifactSubjectV1::FileHunk {
            normalized_relative_path,
            before_file_digest,
            after_file_digest,
            ..
        } = &artifact.subject
        else {
            bail!("R51.3 executable layers support only file hunk artifacts");
        };
        if !artifact_paths.insert(normalized_relative_path.clone()) {
            bail!("Intent layer repeats an artifact file path");
        }
        let change_set_id = artifact
            .provenance
            .changeset_id
            .as_deref()
            .context("Intent layer artifact has no ChangeSet lineage")?;
        let change_set = execution
            .changeset_ids
            .iter()
            .find(|id| id.as_str() == change_set_id)
            .and_then(|id| facts.changesets.get(id))
            .context("Intent layer artifact references another execution ChangeSet")?;
        let change_set_result = execution
            .changeset_ids
            .iter()
            .find(|id| id.as_str() == change_set_id)
            .and_then(|id| facts.changeset_results.get(id))
            .context("Intent layer artifact has no ChangeSet result")?;
        let file = change_set
            .value
            .files
            .iter()
            .find(|file| file.path == *normalized_relative_path)
            .context("Intent layer artifact path is absent from its ChangeSet")?;
        if change_set.stream_sequence >= artifact_sequence
            || change_set_result.stream_sequence >= artifact_sequence
            || change_set_result.value.status != ChangeSetResultStatus::Applied
            || !change_set_file_is_applied(&change_set_result.value, file)
        {
            bail!("Intent layer artifact precedes its applied ChangeSet evidence");
        }
        if !change_set_hashes_match(file, before_file_digest, after_file_digest) {
            bail!("Intent layer artifact hashes do not match its ChangeSet");
        }
        let (prepared, committed) = facts
            .mutation_pair_for_file(execution, file, &accepted.plan.workspace_id)
            .context("Intent layer artifact has no exact mutation pair")?;
        let mutation_identity = prepared
            .value
            .batch_id
            .as_deref()
            .unwrap_or(prepared.value.operation_id.as_str());
        if committed.event_id != artifact.provenance.source_event_id
            || artifact.provenance.mutation_batch_id.as_deref() != Some(mutation_identity)
            || committed.stream_sequence >= artifact_sequence
            || artifact_sequence >= layer_sequence
        {
            bail!("Intent layer artifact provenance does not match its exact mutation");
        }
    }
    if artifact_paths != expected_paths {
        bail!("Intent layer artifacts do not cover the exact execution ChangeSets");
    }
    Ok(())
}

fn mutation_subject_matches_path(subject: &MutationSubject, expected_path: &str) -> bool {
    matches!(
        subject,
        MutationSubject::File { path, file_type }
            if *file_type == FileType::File
                && crate::mutation::portable_relative_path(path).as_deref() == Some(expected_path)
    )
}

fn change_set_hashes_match(
    file: &ChangeSetFile,
    before_digest: &IntentContentDigest,
    after_digest: &IntentContentDigest,
) -> bool {
    let empty = IntentContentDigest::from_bytes([]);
    let before_matches = match file.action {
        ChangeSetFileAction::Create => file.before_hash.is_none() && before_digest == &empty,
        ChangeSetFileAction::Update | ChangeSetFileAction::Delete => {
            file.before_hash.as_deref() == Some(before_digest.as_str())
        }
        ChangeSetFileAction::Rename => false,
    };
    let after_matches = match file.action {
        ChangeSetFileAction::Create | ChangeSetFileAction::Update => {
            file.after_hash.as_deref() == Some(after_digest.as_str())
        }
        ChangeSetFileAction::Delete => file.after_hash.is_none() && after_digest == &empty,
        ChangeSetFileAction::Rename => false,
    };
    before_matches && after_matches
}

fn change_set_file_is_applied(result: &ChangeSetResult, file: &ChangeSetFile) -> bool {
    result.file_results.iter().any(|file_result| {
        file_result.path == file.path
            && file_result.action == file.action
            && file_result.status == crate::ChangeSetFileResultStatus::Applied
    })
}

#[derive(Debug, Clone)]
struct LayerEventFact<T> {
    event_id: String,
    stream_sequence: u64,
    value: T,
}

#[derive(Debug, Default)]
struct LayerMaterializationFacts {
    changesets: BTreeMap<ChangeSetId, LayerEventFact<ChangeSet>>,
    changeset_results: BTreeMap<ChangeSetId, LayerEventFact<ChangeSetResult>>,
    prepared: Vec<LayerEventFact<MutationPrepared>>,
    committed: Vec<LayerEventFact<MutationCommitted>>,
    batch_terminals: Vec<LayerEventFact<MutationBatchFinished>>,
    task_attempts: Vec<LayerEventFact<TaskParticipantAttemptEntry>>,
    promotions: Vec<LayerEventFact<IntegrationPromotionRecorded>>,
    completed_runs: Vec<(String, u64)>,
}

impl LayerMaterializationFacts {
    fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let mut facts = Self::default();
        for record in records {
            let event = record.stored_event();
            let event_id = event.event_id.clone();
            let stream_sequence = event.stream_sequence;
            match event.event_kind() {
                Some(DurableEventType::MutationPrepared) => {
                    facts.prepared.push(LayerEventFact {
                        event_id,
                        stream_sequence,
                        value: serde_json::from_value(event.payload.clone())
                            .context("failed to decode layer mutation prepare")?,
                    });
                    continue;
                }
                Some(DurableEventType::MutationCommitted) => {
                    facts.committed.push(LayerEventFact {
                        event_id,
                        stream_sequence,
                        value: serde_json::from_value(event.payload.clone())
                            .context("failed to decode layer mutation commit")?,
                    });
                    continue;
                }
                Some(DurableEventType::MutationBatchFinished) => {
                    facts.batch_terminals.push(LayerEventFact {
                        event_id,
                        stream_sequence,
                        value: serde_json::from_value(event.payload.clone())
                            .context("failed to decode layer mutation batch terminal")?,
                    });
                    continue;
                }
                Some(DurableEventType::RunFinalized) => {
                    let typed = conversation_run_lifecycle_record_from_stream(record)?;
                    match typed {
                        Some(ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(
                            finalized,
                        )) if finalized.status() == ConversationRunTerminalStatusV1::Succeeded => {
                            facts
                                .completed_runs
                                .push((finalized.run_id().to_owned(), stream_sequence));
                        }
                        Some(_) => {}
                        None => {}
                    }
                    continue;
                }
                _ => {}
            }
            let Some(SessionLogEntry::Control(control)) = record.session_log_entry()? else {
                continue;
            };
            match control {
                ControlEntry::ChangeSetProposed(value) => {
                    let id = value.id.clone();
                    if facts
                        .changesets
                        .insert(
                            id,
                            LayerEventFact {
                                event_id,
                                stream_sequence,
                                value,
                            },
                        )
                        .is_some()
                    {
                        bail!("durable stream repeats a ChangeSet proposal");
                    }
                }
                ControlEntry::ChangeSetApplied(value) => {
                    let id = value.id.clone();
                    if facts
                        .changeset_results
                        .insert(
                            id,
                            LayerEventFact {
                                event_id,
                                stream_sequence,
                                value,
                            },
                        )
                        .is_some()
                    {
                        bail!("durable stream repeats a ChangeSet result");
                    }
                }
                ControlEntry::TaskParticipantAttempt(value) => {
                    facts.task_attempts.push(LayerEventFact {
                        event_id,
                        stream_sequence,
                        value,
                    });
                }
                ControlEntry::IntegrationPromotionRecorded(value) => {
                    facts.promotions.push(LayerEventFact {
                        event_id,
                        stream_sequence,
                        value,
                    });
                }
                _ => {}
            }
        }
        Ok(facts)
    }

    fn terminal_attempt_sequence(&self, execution: &IntentExecutionLineageV1) -> Result<u64> {
        match &execution.binding.origin {
            IntentExecutionOriginV1::Task {
                task_id,
                task_plan_version,
                step_id,
                attempt_id,
            } => {
                let attempt_id = attempt_id
                    .as_deref()
                    .context("Intent layer requires a concrete Task attempt")?;
                self.task_attempts
                    .iter()
                    .find(|fact| {
                        fact.stream_sequence > execution.binding_stream_sequence
                            && fact.value.attempt_id.as_str() == attempt_id
                            && fact.value.task_id.as_str() == task_id
                            && fact.value.plan_version == Some(*task_plan_version)
                            && fact.value.step_id.as_ref().map(crate::TaskStepId::as_str)
                                == Some(step_id.as_str())
                            && fact.value.status == TaskParticipantAttemptStatus::Completed
                    })
                    .map(|fact| fact.stream_sequence)
                    .context("Intent layer requires a completed Task participant attempt")
            }
            IntentExecutionOriginV1::Chat {
                root_logical_run_id,
                attempt_id,
                ..
            } => {
                attempt_id
                    .as_deref()
                    .context("Intent layer requires a concrete Chat attempt")?;
                self.completed_runs
                    .iter()
                    .find(|(run_id, sequence)| {
                        *sequence > execution.binding_stream_sequence
                            && run_id == root_logical_run_id
                    })
                    .map(|(_, sequence)| *sequence)
                    .context("Intent layer requires a successful Chat run terminal")
            }
        }
    }

    fn validate_terminal_attempt(&self, execution: &IntentExecutionLineageV1) -> Result<()> {
        self.terminal_attempt_sequence(execution).map(|_| ())
    }

    fn base_snapshot_id(
        &self,
        execution: &IntentExecutionLineageV1,
        workspace_id: &str,
        before_sequence: Option<u64>,
    ) -> Result<String> {
        match &execution.binding.origin {
            IntentExecutionOriginV1::Task { .. } => {
                let parent_event_id = execution
                    .parent_mutation_event_id
                    .as_deref()
                    .context("Task Intent layer has no parent mutation batch")?;
                let batch = self
                    .batch_terminals
                    .iter()
                    .find(|fact| {
                        fact.event_id == parent_event_id
                            && fact.value.status == MutationBatchStatus::Applied
                    })
                    .context("Task Intent layer parent mutation batch is not applied")?;
                let promotion = self
                    .promotions
                    .iter()
                    .find(|fact| {
                        fact.stream_sequence > batch.stream_sequence
                            && before_sequence
                                .is_none_or(|sequence| fact.stream_sequence < sequence)
                            && fact.value.status == IntegrationPromotionStatus::Promoted
                            && matches!(
                                (&fact.value.target, &fact.value.effect),
                                (
                                    IntegrationPromotionTarget::WorkspaceApply { .. },
                                    Some(IntegrationPromotionEffect::WorkspaceApplied {
                                        promoted_snapshot_id,
                                        ..
                                    })
                                ) if Some(promoted_snapshot_id.as_str())
                                    == execution.parent_snapshot_id.as_deref()
                            )
                    })
                    .context("Task Intent layer has no matching WorkspaceApply promotion")?;
                let IntegrationPromotionTarget::WorkspaceApply {
                    expected_snapshot_id,
                    ..
                } = &promotion.value.target
                else {
                    unreachable!("matching promotion is WorkspaceApply");
                };
                Ok(expected_snapshot_id.clone())
            }
            IntentExecutionOriginV1::Chat { .. } => {
                let mut entries = Vec::new();
                for change_set_id in &execution.changeset_ids {
                    let change_set = self
                        .changesets
                        .get(change_set_id)
                        .context("Chat Intent layer ChangeSet is unavailable")?;
                    for file in &change_set.value.files {
                        let (prepared, committed) = self
                            .mutation_pair_for_file(execution, file, workspace_id)
                            .context("Chat Intent layer has no exact mutation pair")?;
                        if before_sequence.is_some_and(|sequence| {
                            prepared.stream_sequence >= sequence
                                || committed.stream_sequence >= sequence
                        }) {
                            bail!("Chat Intent layer precedes its mutation evidence");
                        }
                        entries.push((
                            file.path.clone(),
                            prepared.value.before_hash.clone(),
                            prepared.value.base_workspace_revision,
                        ));
                    }
                }
                entries.sort();
                let value = serde_json::json!({
                    "workspace_id": workspace_id,
                    "touched_file_base": entries,
                });
                let digest = Sha256::digest(canonical_json_bytes(&value)?);
                Ok(format!("sha256:jcs-v1:{digest:x}"))
            }
        }
    }

    fn mutation_pair_for_file(
        &self,
        execution: &IntentExecutionLineageV1,
        file: &ChangeSetFile,
        workspace_id: &str,
    ) -> Option<(
        &LayerEventFact<MutationPrepared>,
        &LayerEventFact<MutationCommitted>,
    )> {
        let task_batch = match execution.binding.origin {
            IntentExecutionOriginV1::Task { .. } => execution
                .parent_mutation_event_id
                .as_ref()
                .and_then(|event_id| {
                    self.batch_terminals
                        .iter()
                        .find(|fact| &fact.event_id == event_id)
                })
                .map(|fact| (fact.value.batch_id.as_str(), fact.stream_sequence)),
            IntentExecutionOriginV1::Chat { .. } => None,
        };
        self.prepared.iter().find_map(|prepared| {
            if prepared.stream_sequence <= execution.binding_stream_sequence
                || prepared.value.workspace_id != workspace_id
                || prepared.value.before_hash != file.before_hash
                || prepared.value.intended_after_hash != file.after_hash
                || !mutation_subject_matches_path(&prepared.value.subject, &file.path)
                || task_batch.is_some_and(|(batch_id, terminal_sequence)| {
                    prepared.value.batch_id.as_deref() != Some(batch_id)
                        || prepared.stream_sequence >= terminal_sequence
                })
            {
                return None;
            }
            self.committed
                .iter()
                .find(|committed| {
                    committed.stream_sequence > prepared.stream_sequence
                        && committed.value.operation_id == prepared.value.operation_id
                        && committed.value.batch_id == prepared.value.batch_id
                        && committed.value.workspace_id.as_deref() == Some(workspace_id)
                        && committed.value.observed_after_hash == file.after_hash
                        && mutation_subject_matches_path(
                            &committed.value.committed_subject,
                            &file.path,
                        )
                        && task_batch.is_none_or(|(_, terminal_sequence)| {
                            committed.stream_sequence < terminal_sequence
                        })
                        && match execution.binding.origin {
                            IntentExecutionOriginV1::Task { .. } => true,
                            IntentExecutionOriginV1::Chat { .. } => execution
                                .parent_mutation_event_id
                                .as_deref()
                                .is_some_and(|event_id| event_id == committed.event_id),
                        }
                })
                .map(|committed| (prepared, committed))
        })
    }
}

#[derive(Debug)]
struct MaterializedIntentFile {
    path: String,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
    before_digest: IntentContentDigest,
    after_digest: IntentContentDigest,
    old_range: IntentByteRangeV1,
    new_range: IntentByteRangeV1,
    old_content_digest: IntentContentDigest,
    new_content_digest: IntentContentDigest,
    old_changed_bytes: Vec<u8>,
    new_changed_bytes: Vec<u8>,
    source_event_id: String,
    mutation_identity: String,
    changeset_id: String,
}

#[derive(Debug, Clone, Copy)]
enum PatchDirection {
    Forward,
    Reverse,
}

/// Strictly decoded full-file target from one content-addressed Intent patch.
///
/// This stays crate-private because renderer and provider adapters must never receive raw target
/// bytes or turn them into mutation authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactIntentPatchFileV1 {
    pub path: String,
    pub expected_present: bool,
    pub expected_digest: IntentContentDigest,
    pub target: Option<Vec<u8>>,
}

/// Decodes the canonical patch format written by R51.3.
///
/// The decoder rejects duplicate/unsafe paths, invalid flags or digests, oversized targets and
/// trailing bytes. Callers must still bind every entry to the durable layer manifest before use.
pub(crate) fn decode_exact_intent_patch(bytes: &[u8]) -> Result<Vec<ExactIntentPatchFileV1>> {
    let mut input = bytes;
    if take_exact(&mut input, INTENT_PATCH_MAGIC.len())? != INTENT_PATCH_MAGIC {
        bail!("Intent patch has an invalid format marker");
    }
    let file_count = usize::try_from(read_u32(&mut input)?)
        .context("Intent patch file count cannot fit this platform")?;
    if file_count == 0 || file_count > MAX_INTENT_PATCH_FILES {
        bail!("Intent patch file count is outside the supported range");
    }
    let mut paths = BTreeSet::new();
    let mut files = Vec::with_capacity(file_count);
    for _ in 0..file_count {
        let path = String::from_utf8(read_bytes(&mut input, 4_096)?)
            .context("Intent patch path is not UTF-8")?;
        if !normalized_intent_path(&path) || !paths.insert(path.clone()) {
            bail!("Intent patch contains an invalid or duplicate path");
        }
        let expected_present = read_flag(&mut input)?;
        let expected_digest = IntentContentDigest::new(
            String::from_utf8(read_bytes(&mut input, 128)?)
                .context("Intent patch expected digest is not UTF-8")?,
        )?;
        let target = if read_flag(&mut input)? {
            Some(read_bytes(
                &mut input,
                usize::try_from(MAX_WORKSPACE_SNAPSHOT_FILE_BYTES).unwrap_or(usize::MAX),
            )?)
        } else {
            None
        };
        files.push(ExactIntentPatchFileV1 {
            path,
            expected_present,
            expected_digest,
            target,
        });
    }
    if !input.is_empty() {
        bail!("Intent patch contains trailing bytes");
    }
    Ok(files)
}

/// Materializes one exact execution into content-addressed patch/hunk artifacts and appends the
/// artifact manifest immediately before the final layer manifest.
///
/// # Errors
///
/// Returns an error for stale or forged lineage, non-terminal execution, workspace mismatch,
/// unsafe content, unsupported file kinds, or durable append conflict.
pub fn materialize_intent_layer(
    session: &Session,
    workspace_root: impl AsRef<Path>,
    execution_id: &IntentExecutionId,
    redactor: &SecretRedactor,
) -> Result<IntentLayerMaterializationOutcomeV1> {
    let workspace_root = fs::canonicalize(workspace_root.as_ref()).with_context(|| {
        format!(
            "failed to canonicalize Intent layer workspace {}",
            workspace_root.as_ref().display()
        )
    })?;
    let store = session
        .durable_store()
        .context("Intent layer materialization requires a durable session")?;
    let recorder = session
        .mutation_event_recorder()
        .context("Intent layer materialization requires mutation artifacts")?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let admission = IntentStackProjectionV1::from_records(&records)?;
    let accepted = admission
        .latest_accepted_plan()
        .context("Intent layer materialization requires an accepted IntentPlan")?;
    if stable_workspace_id(&workspace_root)? != accepted.plan.workspace_id {
        bail!("Intent layer workspace identity does not match the accepted plan");
    }
    let lineage = crate::IntentLineageProjectionV1::from_records(&records, &admission)?;
    let existing_layers = IntentLayerProjectionV1::from_records(&records, &admission, &lineage)?;
    if let Some(existing) = existing_layers.layer(execution_id) {
        return Ok(IntentLayerMaterializationOutcomeV1 {
            appended: false,
            execution_id: execution_id.clone(),
            manifest_digest: Some(existing.layer_manifest.manifest_digest.as_str().to_owned()),
            read_only_reason: None,
        });
    }
    let execution = lineage
        .execution(execution_id)
        .context("Intent layer references an unknown execution")?;
    if execution.stack_version != accepted.plan.stack_version
        || !accepted
            .plan
            .intents
            .iter()
            .any(|intent| intent.intent_ref == execution.binding.intent_ref)
    {
        bail!("Intent layer materialization cannot use superseded execution authority");
    }
    if execution.read_only_reason.is_some()
        || execution.changeset_ids.is_empty()
        || execution.parent_mutation_event_id.is_none()
        || execution.parent_snapshot_id.is_none()
    {
        return Ok(read_only_materialization(
            execution_id,
            IntentLayerReadOnlyReasonV1::MissingMutationArtifact,
        ));
    }
    let facts = LayerMaterializationFacts::from_records(&records)?;
    if facts.validate_terminal_attempt(execution).is_err() {
        return Ok(read_only_materialization(
            execution_id,
            IntentLayerReadOnlyReasonV1::MissingTerminalAttempt,
        ));
    }
    let files = match materialize_execution_files(
        &workspace_root,
        &recorder,
        &facts,
        execution,
        &accepted.plan.workspace_id,
        redactor,
    )? {
        MaterializedFiles::Ready(files) => files,
        MaterializedFiles::ReadOnly(reason) => {
            return Ok(read_only_materialization(execution_id, reason));
        }
    };
    let base_snapshot_id = facts.base_snapshot_id(execution, &accepted.plan.workspace_id, None)?;
    let operation_id = format!("intent-layer-{}", execution_id.as_str());
    let forward_bytes = encode_patch(&files, PatchDirection::Forward)?;
    let reverse_bytes = encode_patch(&files, PatchDirection::Reverse)?;
    let forward_patch_artifact_id = recorder.capture_immutable_content_artifact(
        &accepted.plan.workspace_id,
        &operation_id,
        Path::new("intent/forward.patch"),
        &forward_bytes,
    )?;
    let reverse_patch_artifact_id = recorder.capture_immutable_content_artifact(
        &accepted.plan.workspace_id,
        &operation_id,
        Path::new("intent/reverse.patch"),
        &reverse_bytes,
    )?;
    let mut core = IntentLayerCoreV1 {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        intent_ref: execution.binding.intent_ref.clone(),
        execution_id: execution_id.clone(),
        execution_origin: execution.binding.origin.clone(),
        base_snapshot_id,
        result_snapshot_id: execution
            .parent_snapshot_id
            .clone()
            .context("Intent layer result snapshot is unavailable")?,
        changeset_ids: execution
            .changeset_ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect(),
        operation_ids: Vec::new(),
        forward_patch_artifact_id,
        reverse_patch_artifact_id,
        core_digest: zero_intent_digest()?,
    };
    core.core_digest = core.computed_digest()?;

    let mut bindings = Vec::with_capacity(files.len());
    for file in &files {
        let hunk_bytes = encode_hunk(file, &core.core_digest)?;
        let artifact_id = recorder.capture_immutable_content_artifact(
            &accepted.plan.workspace_id,
            &operation_id,
            Path::new(&file.path),
            &hunk_bytes,
        )?;
        bindings.push(IntentArtifactBindingV1 {
            artifact_id: IntentArtifactId::new(artifact_id)?,
            artifact_kind: IntentArtifactKind::FileHunk,
            subject: BoundedIntentArtifactSubjectV1::FileHunk {
                normalized_relative_path: file.path.clone(),
                before_file_digest: file.before_digest.clone(),
                after_file_digest: file.after_digest.clone(),
                old_range: file.old_range,
                new_range: file.new_range,
                old_content_digest: file.old_content_digest.clone(),
                new_content_digest: file.new_content_digest.clone(),
                layer_core_digest: core.core_digest.clone(),
            },
            ownership: IntentArtifactOwnership::Exclusive,
            availability: IntentArtifactAvailability::Available,
            before_digest: Some(file.old_content_digest.clone()),
            after_digest: file.new_content_digest.clone(),
            provenance: IntentArtifactProvenanceV1 {
                source_event_id: file.source_event_id.clone(),
                execution_id: execution_id.clone(),
                mutation_batch_id: Some(file.mutation_identity.clone()),
                changeset_id: Some(file.changeset_id.clone()),
                verification_receipt_id: None,
            },
        });
    }
    bindings.sort_by(|left, right| {
        artifact_path(left)
            .cmp(artifact_path(right))
            .then_with(|| left.artifact_id.cmp(&right.artifact_id))
    });
    let mut artifact_manifest = IntentArtifactManifestV1 {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        intent_ref: execution.binding.intent_ref.clone(),
        execution_id: execution_id.clone(),
        layer_core_digest: core.core_digest.clone(),
        artifacts: bindings,
        manifest_digest: zero_intent_digest()?,
    };
    artifact_manifest.manifest_digest = artifact_manifest.computed_digest()?;
    let artifact_manifest_bytes = canonical_json_bytes(&serde_json::to_value(&artifact_manifest)?)?;
    let artifact_manifest_id = recorder.capture_immutable_content_artifact(
        &accepted.plan.workspace_id,
        &operation_id,
        Path::new("intent/artifact-manifest.json"),
        &artifact_manifest_bytes,
    )?;
    let mut layer_manifest = IntentLayerManifestV1 {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        core,
        artifact_manifest_id,
        artifact_manifest_digest: artifact_manifest.manifest_digest.clone(),
        manifest_digest: zero_intent_digest()?,
    };
    layer_manifest.manifest_digest = layer_manifest.computed_digest()?;
    let appended = append_layer_pair(&store, artifact_manifest.clone(), layer_manifest.clone())?;
    Ok(IntentLayerMaterializationOutcomeV1 {
        appended,
        execution_id: execution_id.clone(),
        manifest_digest: Some(layer_manifest.manifest_digest.as_str().to_owned()),
        read_only_reason: None,
    })
}

enum MaterializedFiles {
    Ready(Vec<MaterializedIntentFile>),
    ReadOnly(IntentLayerReadOnlyReasonV1),
}

fn materialize_execution_files(
    workspace_root: &Path,
    recorder: &MutationEventRecorder,
    facts: &LayerMaterializationFacts,
    execution: &IntentExecutionLineageV1,
    workspace_id: &str,
    redactor: &SecretRedactor,
) -> Result<MaterializedFiles> {
    let mut seen_paths = BTreeSet::new();
    let mut materialized = Vec::new();
    for change_set_id in &execution.changeset_ids {
        let change_set = facts
            .changesets
            .get(change_set_id)
            .context("Intent layer ChangeSet proposal is unavailable")?;
        let Some(change_set_result) = facts.changeset_results.get(change_set_id) else {
            return Ok(MaterializedFiles::ReadOnly(
                IntentLayerReadOnlyReasonV1::MissingMutationArtifact,
            ));
        };
        if change_set_result.value.status != ChangeSetResultStatus::Applied {
            return Ok(MaterializedFiles::ReadOnly(
                IntentLayerReadOnlyReasonV1::MissingMutationArtifact,
            ));
        }
        for file in &change_set.value.files {
            if file.action == ChangeSetFileAction::Rename || !seen_paths.insert(file.path.clone()) {
                return Ok(MaterializedFiles::ReadOnly(
                    IntentLayerReadOnlyReasonV1::UnownedArtifact,
                ));
            }
            if !normalized_intent_path(&file.path) {
                return Ok(MaterializedFiles::ReadOnly(
                    IntentLayerReadOnlyReasonV1::UnownedArtifact,
                ));
            }
            if is_sensitive_mutation_artifact_path(Path::new(&file.path)) {
                return Ok(MaterializedFiles::ReadOnly(
                    IntentLayerReadOnlyReasonV1::SensitiveContent,
                ));
            }
            if !change_set_file_is_applied(&change_set_result.value, file) {
                return Ok(MaterializedFiles::ReadOnly(
                    IntentLayerReadOnlyReasonV1::MissingMutationArtifact,
                ));
            }
            let Some((prepared, committed)) =
                facts.mutation_pair_for_file(execution, file, workspace_id)
            else {
                return Ok(MaterializedFiles::ReadOnly(
                    IntentLayerReadOnlyReasonV1::MissingMutationArtifact,
                ));
            };
            let before = match (&prepared.value.snapshot_coverage, file.action) {
                (SnapshotCoverage::Captured(artifact_id), _) => {
                    let bytes = match recorder.read_immutable_content_artifact(artifact_id) {
                        Ok(bytes) => bytes,
                        Err(_) => {
                            return Ok(MaterializedFiles::ReadOnly(
                                IntentLayerReadOnlyReasonV1::ArtifactUnavailable,
                            ));
                        }
                    };
                    if bytes.len() as u64 > MAX_WORKSPACE_SNAPSHOT_FILE_BYTES {
                        return Ok(MaterializedFiles::ReadOnly(
                            IntentLayerReadOnlyReasonV1::UnsupportedFileKind,
                        ));
                    }
                    Some(bytes)
                }
                (SnapshotCoverage::NoPriorContent, ChangeSetFileAction::Create) => None,
                (
                    SnapshotCoverage::SkippedSensitive,
                    ChangeSetFileAction::Create
                    | ChangeSetFileAction::Update
                    | ChangeSetFileAction::Delete,
                ) => {
                    return Ok(MaterializedFiles::ReadOnly(
                        IntentLayerReadOnlyReasonV1::SensitiveContent,
                    ));
                }
                _ => {
                    return Ok(MaterializedFiles::ReadOnly(
                        IntentLayerReadOnlyReasonV1::MissingMutationArtifact,
                    ));
                }
            };
            let absolute_path = workspace_root.join(&file.path);
            let parent = absolute_path
                .parent()
                .context("Intent layer file has no parent directory")?;
            let canonical_parent = fs::canonicalize(parent).with_context(|| {
                format!(
                    "failed to canonicalize Intent layer parent {}",
                    parent.display()
                )
            })?;
            if !canonical_parent.starts_with(workspace_root) {
                return Ok(MaterializedFiles::ReadOnly(
                    IntentLayerReadOnlyReasonV1::UnsupportedFileKind,
                ));
            }
            let after = match file.action {
                ChangeSetFileAction::Create | ChangeSetFileAction::Update => {
                    let metadata = fs::symlink_metadata(&absolute_path).with_context(|| {
                        format!(
                            "failed to inspect Intent layer file {}",
                            absolute_path.display()
                        )
                    })?;
                    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                        return Ok(MaterializedFiles::ReadOnly(
                            IntentLayerReadOnlyReasonV1::UnsupportedFileKind,
                        ));
                    }
                    if metadata.len() > MAX_WORKSPACE_SNAPSHOT_FILE_BYTES {
                        return Ok(MaterializedFiles::ReadOnly(
                            IntentLayerReadOnlyReasonV1::UnsupportedFileKind,
                        ));
                    }
                    let canonical_path = fs::canonicalize(&absolute_path).with_context(|| {
                        format!(
                            "failed to canonicalize Intent layer file {}",
                            absolute_path.display()
                        )
                    })?;
                    if !canonical_path.starts_with(workspace_root) {
                        return Ok(MaterializedFiles::ReadOnly(
                            IntentLayerReadOnlyReasonV1::UnsupportedFileKind,
                        ));
                    }
                    Some(fs::read(&canonical_path).with_context(|| {
                        format!(
                            "failed to read Intent layer file {}",
                            canonical_path.display()
                        )
                    })?)
                }
                ChangeSetFileAction::Delete => {
                    match fs::symlink_metadata(&absolute_path) {
                        Ok(_) => {
                            return Ok(MaterializedFiles::ReadOnly(
                                IntentLayerReadOnlyReasonV1::DriftedArtifact,
                            ));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(error).with_context(|| {
                                format!(
                                    "failed to inspect deleted Intent layer file {}",
                                    absolute_path.display()
                                )
                            });
                        }
                    }
                    None
                }
                ChangeSetFileAction::Rename => unreachable!("rename returned read-only above"),
            };
            if !digest_matches(before.as_deref(), file.before_hash.as_deref())
                || !digest_matches(after.as_deref(), file.after_hash.as_deref())
                || committed.value.observed_after_hash != file.after_hash
            {
                return Ok(MaterializedFiles::ReadOnly(
                    IntentLayerReadOnlyReasonV1::DriftedArtifact,
                ));
            }
            let before_text = before
                .as_deref()
                .map(std::str::from_utf8)
                .transpose()
                .ok()
                .flatten();
            let after_text = after
                .as_deref()
                .map(std::str::from_utf8)
                .transpose()
                .ok()
                .flatten();
            if (before.is_some() && before_text.is_none())
                || (after.is_some() && after_text.is_none())
            {
                return Ok(MaterializedFiles::ReadOnly(
                    IntentLayerReadOnlyReasonV1::UnsupportedFileKind,
                ));
            }
            if before_text.is_some_and(|text| redactor.text_contains_secret(text))
                || after_text.is_some_and(|text| redactor.text_contains_secret(text))
            {
                return Ok(MaterializedFiles::ReadOnly(
                    IntentLayerReadOnlyReasonV1::SensitiveContent,
                ));
            }
            let before_bytes = before.as_deref().unwrap_or_default();
            let after_bytes = after.as_deref().unwrap_or_default();
            let (old_range, new_range) = canonical_changed_ranges(before_bytes, after_bytes)?;
            let old_changed_bytes =
                before_bytes[old_range.start as usize..old_range.end as usize].to_vec();
            let new_changed_bytes =
                after_bytes[new_range.start as usize..new_range.end as usize].to_vec();
            materialized.push(MaterializedIntentFile {
                path: file.path.clone(),
                before_digest: IntentContentDigest::from_bytes(before_bytes),
                after_digest: IntentContentDigest::from_bytes(after_bytes),
                old_content_digest: IntentContentDigest::from_bytes(&old_changed_bytes),
                new_content_digest: IntentContentDigest::from_bytes(&new_changed_bytes),
                old_range,
                new_range,
                old_changed_bytes,
                new_changed_bytes,
                before,
                after,
                source_event_id: committed.event_id.clone(),
                mutation_identity: prepared
                    .value
                    .batch_id
                    .clone()
                    .unwrap_or_else(|| prepared.value.operation_id.clone()),
                changeset_id: change_set_id.as_str().to_owned(),
            });
        }
    }
    materialized.sort_by(|left, right| left.path.cmp(&right.path));
    if materialized.is_empty() {
        return Ok(MaterializedFiles::ReadOnly(
            IntentLayerReadOnlyReasonV1::MissingMutationArtifact,
        ));
    }
    Ok(MaterializedFiles::Ready(materialized))
}

fn canonical_changed_ranges(
    before: &[u8],
    after: &[u8],
) -> Result<(IntentByteRangeV1, IntentByteRangeV1)> {
    if before == after {
        bail!("Intent layer cannot materialize a no-op file delta");
    }
    let before_text =
        std::str::from_utf8(before).context("Intent layer before content is not UTF-8")?;
    let after_text =
        std::str::from_utf8(after).context("Intent layer after content is not UTF-8")?;
    let mut prefix = before
        .iter()
        .zip(after.iter())
        .take_while(|(left, right)| left == right)
        .count();
    while !before_text.is_char_boundary(prefix) || !after_text.is_char_boundary(prefix) {
        prefix = prefix.saturating_sub(1);
    }
    let max_suffix = before.len().min(after.len()).saturating_sub(prefix);
    let mut suffix = before
        .iter()
        .rev()
        .zip(after.iter().rev())
        .take(max_suffix)
        .take_while(|(left, right)| left == right)
        .count();
    while !before_text.is_char_boundary(before.len().saturating_sub(suffix))
        || !after_text.is_char_boundary(after.len().saturating_sub(suffix))
    {
        suffix = suffix.saturating_sub(1);
    }
    Ok((
        IntentByteRangeV1 {
            start: prefix as u64,
            end: before.len().saturating_sub(suffix) as u64,
        },
        IntentByteRangeV1 {
            start: prefix as u64,
            end: after.len().saturating_sub(suffix) as u64,
        },
    ))
}

fn digest_matches(bytes: Option<&[u8]>, expected: Option<&str>) -> bool {
    match (bytes, expected) {
        (None, None) => true,
        (Some(bytes), Some(expected)) => IntentContentDigest::from_bytes(bytes)
            .as_str()
            .eq_ignore_ascii_case(expected),
        _ => false,
    }
}

fn encode_patch(files: &[MaterializedIntentFile], direction: PatchDirection) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    output.extend_from_slice(INTENT_PATCH_MAGIC);
    push_u32(&mut output, files.len())?;
    for file in files {
        push_bytes(&mut output, file.path.as_bytes())?;
        let (expected, expected_present, target) = match direction {
            PatchDirection::Forward => (
                &file.before_digest,
                file.before.is_some(),
                file.after.as_deref(),
            ),
            PatchDirection::Reverse => (
                &file.after_digest,
                file.after.is_some(),
                file.before.as_deref(),
            ),
        };
        output.push(u8::from(expected_present));
        push_bytes(&mut output, expected.as_str().as_bytes())?;
        match target {
            Some(bytes) => {
                output.push(1);
                push_bytes(&mut output, bytes)?;
            }
            None => output.push(0),
        }
    }
    Ok(output)
}

fn encode_hunk(
    file: &MaterializedIntentFile,
    core_digest: &crate::IntentDigest,
) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    output.extend_from_slice(INTENT_HUNK_MAGIC);
    push_bytes(&mut output, file.path.as_bytes())?;
    push_bytes(&mut output, core_digest.as_str().as_bytes())?;
    push_u64(&mut output, file.old_range.start);
    push_u64(&mut output, file.old_range.end);
    push_u64(&mut output, file.new_range.start);
    push_u64(&mut output, file.new_range.end);
    push_bytes(&mut output, &file.old_changed_bytes)?;
    push_bytes(&mut output, &file.new_changed_bytes)?;
    Ok(output)
}

fn push_bytes(output: &mut Vec<u8>, value: &[u8]) -> Result<()> {
    push_u64(
        output,
        u64::try_from(value.len()).context("Intent artifact content is too large")?,
    );
    output.extend_from_slice(value);
    Ok(())
}

fn push_u32(output: &mut Vec<u8>, value: usize) -> Result<()> {
    output.extend_from_slice(
        &u32::try_from(value)
            .context("Intent artifact contains too many files")?
            .to_be_bytes(),
    );
    Ok(())
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn read_flag(input: &mut &[u8]) -> Result<bool> {
    match take_exact(input, 1)?[0] {
        0 => Ok(false),
        1 => Ok(true),
        _ => bail!("Intent patch contains an invalid presence flag"),
    }
}

fn read_u32(input: &mut &[u8]) -> Result<u32> {
    let mut bytes = [0_u8; 4];
    bytes.copy_from_slice(take_exact(input, 4)?);
    Ok(u32::from_be_bytes(bytes))
}

fn read_u64(input: &mut &[u8]) -> Result<u64> {
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(take_exact(input, 8)?);
    Ok(u64::from_be_bytes(bytes))
}

fn read_bytes(input: &mut &[u8], maximum: usize) -> Result<Vec<u8>> {
    let length = usize::try_from(read_u64(input)?)
        .context("Intent patch length cannot fit this platform")?;
    if length > maximum {
        bail!("Intent patch field exceeds its bounded size");
    }
    Ok(take_exact(input, length)?.to_vec())
}

fn take_exact<'a>(input: &mut &'a [u8], length: usize) -> Result<&'a [u8]> {
    if input.len() < length {
        bail!("Intent patch is truncated");
    }
    let (value, remaining) = input.split_at(length);
    *input = remaining;
    Ok(value)
}

fn zero_intent_digest() -> Result<crate::IntentDigest> {
    crate::IntentDigest::new(format!(
        "{}{}",
        crate::INTENT_CANONICAL_DIGEST_PREFIX,
        "0".repeat(64)
    ))
}

fn artifact_path(artifact: &IntentArtifactBindingV1) -> &str {
    match &artifact.subject {
        BoundedIntentArtifactSubjectV1::FileHunk {
            normalized_relative_path,
            ..
        } => normalized_relative_path,
        _ => "",
    }
}

fn append_layer_pair(
    store: &JsonlSessionStore,
    artifact_manifest: IntentArtifactManifestV1,
    layer_manifest: IntentLayerManifestV1,
) -> Result<bool> {
    let artifact_event = IntentEventV1::ArtifactBindingsRecorded {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        manifest: artifact_manifest.clone(),
    };
    let layer_event = IntentEventV1::LayerManifestRecorded {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        manifest: layer_manifest.clone(),
    };
    artifact_event.validate_contract()?;
    layer_event.validate_contract()?;
    let expected_execution_id = layer_manifest.core.execution_id.clone();
    let expected_artifacts = artifact_manifest.clone();
    let expected_layer = layer_manifest.clone();
    Ok(store
        .append_events_and_session_entries_if(
            vec![
                (
                    DurableEventType::IntentArtifactBindingsRecorded,
                    EventClass::Critical,
                    serde_json::to_value(artifact_event)?,
                ),
                (
                    DurableEventType::IntentLayerManifestRecorded,
                    EventClass::Critical,
                    serde_json::to_value(layer_event)?,
                ),
            ],
            &[],
            move |records| {
                let admission = IntentStackProjectionV1::from_records(records)?;
                let lineage = crate::IntentLineageProjectionV1::from_records(records, &admission)?;
                let projection =
                    IntentLayerProjectionV1::from_records(records, &admission, &lineage)?;
                if let Some(existing) = projection.layer(&expected_execution_id) {
                    if existing.artifact_manifest != expected_artifacts
                        || existing.layer_manifest != expected_layer
                    {
                        bail!("Intent execution already has a conflicting layer");
                    }
                    return Ok(false);
                }
                let execution = lineage
                    .execution(&expected_execution_id)
                    .context("Intent layer execution disappeared before append")?;
                validate_artifact_manifest_lineage(&expected_artifacts, execution)?;
                Ok(true)
            },
        )?
        .is_some())
}

fn read_only_materialization(
    execution_id: &IntentExecutionId,
    reason: IntentLayerReadOnlyReasonV1,
) -> IntentLayerMaterializationOutcomeV1 {
    IntentLayerMaterializationOutcomeV1 {
        appended: false,
        execution_id: execution_id.clone(),
        manifest_digest: None,
        read_only_reason: Some(reason),
    }
}

fn normalized_intent_path(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains('\\')
        && value
            .split('/')
            .all(|part| !part.is_empty() && !matches!(part, "." | ".."))
}

impl Session {
    /// Rebuilds R51.3 layers and checks their current content-addressed artifacts.
    pub fn intent_layer_projection(&self) -> Result<IntentLayerProjectionV1> {
        let store = self
            .durable_store()
            .context("Intent layer projection requires a durable session")?;
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let admission = IntentStackProjectionV1::from_records(&records)?;
        let lineage = crate::IntentLineageProjectionV1::from_records(&records, &admission)?;
        let mut projection = IntentLayerProjectionV1::from_records(&records, &admission, &lineage)?;
        if let Some(recorder) = self.mutation_event_recorder() {
            projection.refresh_artifact_availability(&recorder)?;
        }
        Ok(projection)
    }
}

#[cfg(test)]
#[path = "tests/intent_layer_tests.rs"]
mod tests;
