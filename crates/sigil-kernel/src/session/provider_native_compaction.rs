use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    provider_attempt::{PhysicalAttemptAppendGuard, append_direct_record_and_sync},
    provider_continuation_payload_coordinator::ProviderContinuationPayloadCoordinatorInner,
    provider_continuation_payload_store::ProviderContinuationSessionKeyStore,
    *,
};
use crate::{EventId, FrozenProviderRequestMaterial};

/// Durable input for one provider-native compaction attempt.
///
/// The caller must freeze the same complete request window that it will send to the provider and
/// identify the durable transcript cursor covered by that window. This type is provider-neutral:
/// wire mapping and opaque response handling remain in the provider crate.
#[derive(Debug)]
pub struct NativeProviderCompactionRequest {
    pub logical_run_id: String,
    pub frozen_request: FrozenProviderRequestMaterial,
    pub covers_through: CompactionCursor,
    pub metadata: NativeProviderCompactionMetadata,
}

/// Durable identities created after one opaque native compaction response is materialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeProviderCompactionMaterialization {
    pub physical_attempt_id: ProviderPhysicalAttemptId,
    pub observation_id: ProviderContinuationObservationId,
    pub candidate_id: ProviderContinuationCandidateId,
    pub payload_id: ProviderContinuationPayloadId,
    pub artifact_id: ProviderContinuationArtifactId,
}

/// A provider-native compaction physical attempt whose start barrier is already durable.
///
/// The caller owns the provider I/O between [`Self::start`] and either
/// [`Self::materialize_artifact`] plus [`Self::finish`], or a truthful terminal recorded with
/// [`Self::finish`]. It never activates a compaction boundary or opens a user-facing operation.
pub struct NativeProviderCompactionAttempt {
    inner: NativeProviderCompactionAttemptInner<ProviderContinuationPayloadCoordinator>,
}

pub(super) struct NativeProviderCompactionAttemptInner<C> {
    store: JsonlSessionStore,
    session_scope_id: String,
    provider_name: String,
    model_name: String,
    frozen_request: FrozenProviderRequestMaterial,
    physical_attempt_id: ProviderPhysicalAttemptId,
    request_material_fingerprint: String,
    start_event_id: EventId,
    last_causation_event_id: EventId,
    durable_output_event_ids: Vec<EventId>,
    covers_through: CompactionCursor,
    metadata: NativeProviderCompactionMetadata,
    coordinator: Option<C>,
    terminal_recorded: bool,
}

pub(super) trait NativeCompactionPayloadCoordinator: Send + 'static {
    fn persist_committed_payload(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
        payload: &[u8],
    ) -> Result<ProviderContinuationPayloadCommitResult>;
}

impl NativeCompactionPayloadCoordinator for ProviderContinuationPayloadCoordinator {
    fn persist_committed_payload(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
        payload: &[u8],
    ) -> Result<ProviderContinuationPayloadCommitResult> {
        Self::persist_committed_payload(self, manifest, payload)
    }
}

impl<K> NativeCompactionPayloadCoordinator for ProviderContinuationPayloadCoordinatorInner<K>
where
    K: ProviderContinuationSessionKeyStore + 'static,
{
    fn persist_committed_payload(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
        payload: &[u8],
    ) -> Result<ProviderContinuationPayloadCommitResult> {
        Self::persist_committed_payload(self, manifest, payload)
    }
}

impl NativeProviderCompactionAttempt {
    /// Appends and syncs the `NativeCompaction` physical-attempt start before provider I/O.
    ///
    /// # Errors
    ///
    /// Returns an error before any provider request when the session is not durable, the frozen
    /// request/cursor/metadata are not bound to this session, or the encrypted payload backend
    /// cannot be opened.
    pub async fn start(
        session: &Session,
        request: NativeProviderCompactionRequest,
    ) -> Result<Self> {
        let store = session
            .durable_store()
            .context("native provider compaction requires a durable session store")?;
        let coordinator = ProviderContinuationPayloadCoordinator::for_store(store.clone())?;
        Ok(Self {
            inner: NativeProviderCompactionAttemptInner::start(
                session,
                store,
                coordinator,
                request,
            )
            .await?,
        })
    }

    /// Returns the immutable provider-neutral request that the caller must send unchanged.
    #[must_use]
    pub fn request(&self) -> &FrozenProviderRequestMaterial {
        &self.inner.frozen_request
    }

    /// Persists an opaque provider-native compacted window through the encrypted payload lifecycle.
    ///
    /// `opaque_payload` must be the provider's complete canonical replacement window. The kernel
    /// authenticates and encrypts the bytes but does not decode, prune, or rewrite them.
    pub async fn materialize_artifact(
        &mut self,
        provider_response_id: impl Into<String>,
        opaque_payload: Vec<u8>,
    ) -> Result<NativeProviderCompactionMaterialization> {
        self.inner
            .materialize_artifact(provider_response_id.into(), opaque_payload)
            .await
    }

    /// Appends the one truthful terminal for this physical attempt.
    pub async fn finish(
        &mut self,
        outcome: ProviderPhysicalAttemptOutcome,
        provider_response_id: Option<String>,
    ) -> Result<()> {
        self.inner.finish(outcome, provider_response_id).await
    }

    /// Returns whether a durable observation or candidate was already appended.
    #[must_use]
    pub fn has_durable_output(&self) -> bool {
        !self.inner.durable_output_event_ids.is_empty()
    }
}

impl<C> NativeProviderCompactionAttemptInner<C>
where
    C: NativeCompactionPayloadCoordinator,
{
    pub(super) async fn start(
        session: &Session,
        store: JsonlSessionStore,
        coordinator: C,
        request: NativeProviderCompactionRequest,
    ) -> Result<Self> {
        let session_scope_id = session.session_scope_id().to_owned();
        if request.frozen_request.session_scope_id() != session_scope_id {
            bail!("native provider compaction request belongs to a different session scope");
        }
        validate_covers_through(&store, &session_scope_id, &request.covers_through)?;
        let frozen_request = request.frozen_request;
        let provider_request = frozen_request.request();
        request.metadata.validate_for_request(
            &provider_request.provider_name,
            &provider_request.model_name,
        )?;

        let physical_attempt_id = format!("native-provider-compaction-{}", Uuid::new_v4());
        let start_event_id = Uuid::new_v4().to_string();
        let entry = ProviderPhysicalAttemptStartedEntry {
            schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
            physical_attempt_id: physical_attempt_id.clone(),
            logical_run_id: request.logical_run_id,
            purpose: ProviderPhysicalAttemptPurpose::NativeCompaction,
            request_material_fingerprint: frozen_request.fingerprint().to_owned(),
            provider_name: provider_request.provider_name.clone(),
            model_name: provider_request.model_name.clone(),
            started_at_unix_ms: unix_time_ms(),
        };
        entry.validate_shape()?;
        append_direct_record_and_sync(
            store.clone(),
            session_scope_id.clone(),
            DurableEventType::ProviderPhysicalAttemptStarted,
            serde_json::to_value(&entry)
                .context("failed to encode native provider compaction start")?,
            physical_attempt_id.clone(),
            start_event_id.clone(),
            Some(start_event_id.clone()),
            None,
            PhysicalAttemptAppendGuard::Start {
                physical_attempt_id: physical_attempt_id.clone(),
            },
        )
        .await?;
        Ok(Self {
            store,
            session_scope_id,
            provider_name: entry.provider_name,
            model_name: entry.model_name,
            frozen_request,
            physical_attempt_id,
            request_material_fingerprint: entry.request_material_fingerprint,
            start_event_id: start_event_id.clone(),
            last_causation_event_id: start_event_id,
            durable_output_event_ids: Vec::new(),
            covers_through: request.covers_through,
            metadata: request.metadata,
            coordinator: Some(coordinator),
            terminal_recorded: false,
        })
    }

    pub(super) async fn materialize_artifact(
        &mut self,
        provider_response_id: String,
        opaque_payload: Vec<u8>,
    ) -> Result<NativeProviderCompactionMaterialization> {
        if self.terminal_recorded {
            bail!("native provider compaction output cannot follow its terminal");
        }
        if opaque_payload.is_empty() {
            bail!("native provider compaction payload must not be empty");
        }
        let observed_payload_integrity_tag = provider_continuation_observed_payload_integrity_tag(
            &self.session_scope_id,
            &opaque_payload,
        )?;
        let observation_id = provider_continuation_observation_id(
            &self.session_scope_id,
            &self.metadata.provider_route_fingerprint,
            &self.physical_attempt_id,
            0,
            &observed_payload_integrity_tag,
        );
        let observation_event_id = provider_continuation_observed_event_id(&observation_id);
        let observed_at_unix_ms = unix_time_ms();
        let observation = ProviderContinuationObservedEntry {
            schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
            observation_id: observation_id.clone(),
            physical_attempt_id: self.physical_attempt_id.clone(),
            response_item_ordinal: 0,
            observed_payload_integrity_tag,
            provider_name: self.provider_name.clone(),
            provider_route_fingerprint: self.metadata.provider_route_fingerprint.clone(),
            model_name: self.model_name.clone(),
            model_metadata_profile: self.metadata.model_metadata_profile.clone(),
            wire_profile: self.metadata.wire_profile.clone(),
            wire_protocol: self.metadata.wire_protocol.clone(),
            wire_schema_version: self.metadata.wire_schema_version.clone(),
            provider_request_id: None,
            provider_response_id: Some(provider_response_id),
            observed_at_unix_ms,
        };
        append_session_bound_direct_record_and_sync(
            self.store.clone(),
            self.session_scope_id.clone(),
            DurableEventType::ProviderContinuationObserved,
            serde_json::to_value(&observation)
                .context("failed to encode native provider compaction observation")?,
            observation_event_id.clone(),
            Some(self.start_event_id.clone()),
            Some(self.last_causation_event_id.clone()),
            self.output_guard(),
        )
        .await?;
        self.record_output(observation_event_id.clone());

        let candidate_id = provider_continuation_candidate_id_from_observation(&observation_id);
        let payload_id = provider_continuation_payload_id(
            &candidate_id,
            ProviderContinuationPayloadKind::Artifact,
        );
        let artifact_id = format!("native-provider-artifact-{}", Uuid::new_v4());
        let integrity = ProviderContinuationPayloadIntegrity::Sha256(format!(
            "sha256:{:x}",
            Sha256::digest(&opaque_payload)
        ));
        let candidate = ProviderContinuationCandidateRecordedEntry {
            schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
            candidate_id: candidate_id.clone(),
            observation_id: Some(observation_id.clone()),
            candidate: ProviderContinuationCandidate::Artifact(ProviderCompactionArtifactRef {
                candidate_id: candidate_id.clone(),
                payload: ProviderContinuationPayloadIdentity {
                    payload_id: payload_id.clone(),
                    integrity: integrity.clone(),
                    byte_size: opaque_payload.len() as u64,
                },
                artifact_id: artifact_id.clone(),
                provider_name: self.provider_name.clone(),
                provider_route_fingerprint: self.metadata.provider_route_fingerprint.clone(),
                model_name: self.model_name.clone(),
                model_metadata_profile: self.metadata.model_metadata_profile.clone(),
                wire_profile: self.metadata.wire_profile.clone(),
                wire_protocol: self.metadata.wire_protocol.clone(),
                wire_schema_version: self.metadata.wire_schema_version.clone(),
                composition_profile: self.metadata.composition_profile.clone(),
                artifact_kind: self.metadata.artifact_kind.clone(),
                composition_mode: ProviderArtifactComposition::ReplacementWindow,
                covers_through: self.covers_through.clone(),
                request_fingerprint: self.request_material_fingerprint.clone(),
                sensitivity: self.metadata.sensitivity,
            }),
            resolution_mode: ProviderContinuationResolutionMode::NativeOnly,
            activation_gate: ProviderContinuationActivationGate::Immediate,
            source_event_id: observation_event_id.clone(),
            created_at_unix_ms: unix_time_ms(),
        };
        let manifest = ProviderContinuationPayloadLifecycleEntry {
            schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
            payload_id: payload_id.clone(),
            candidate_id: candidate_id.clone(),
            source: ProviderContinuationPayloadSource::ProviderObserved {
                observation_event_id: observation_event_id.clone(),
                observation_id: observation_id.clone(),
            },
            kind: ProviderContinuationPayloadKind::Artifact,
            storage_ref: ProviderContinuationPayloadStorageRef::Artifact {
                artifact_id: artifact_id.clone(),
            },
            integrity,
            byte_size: opaque_payload.len() as u64,
            state: ProviderContinuationPayloadLifecycleState::Committed,
            reason: None,
        };
        self.persist_payload(manifest, opaque_payload).await?;

        let candidate_event_id = provider_continuation_candidate_recorded_event_id(&candidate_id);
        append_session_bound_direct_record_and_sync(
            self.store.clone(),
            self.session_scope_id.clone(),
            DurableEventType::ProviderContinuationCandidateRecorded,
            serde_json::to_value(&candidate)
                .context("failed to encode native provider compaction candidate")?,
            candidate_event_id.clone(),
            Some(self.start_event_id.clone()),
            Some(self.last_causation_event_id.clone()),
            self.output_guard(),
        )
        .await?;
        self.record_output(candidate_event_id);
        Ok(NativeProviderCompactionMaterialization {
            physical_attempt_id: self.physical_attempt_id.clone(),
            observation_id,
            candidate_id,
            payload_id,
            artifact_id,
        })
    }

    pub(super) async fn finish(
        &mut self,
        outcome: ProviderPhysicalAttemptOutcome,
        provider_response_id: Option<String>,
    ) -> Result<()> {
        if self.terminal_recorded {
            bail!("native provider compaction terminal was already recorded");
        }
        let entry = ProviderPhysicalAttemptTerminalEntry {
            schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
            physical_attempt_id: self.physical_attempt_id.clone(),
            request_material_fingerprint: self.request_material_fingerprint.clone(),
            outcome,
            rejection: None,
            provider_request_id: None,
            provider_response_id,
            durable_output_event_ids: self.durable_output_event_ids.clone(),
            durable_side_effect_event_ids: Vec::new(),
            finished_at_unix_ms: unix_time_ms(),
        };
        entry.validate_shape()?;
        append_direct_record_and_sync(
            self.store.clone(),
            self.session_scope_id.clone(),
            DurableEventType::ProviderPhysicalAttemptTerminal,
            serde_json::to_value(&entry)
                .context("failed to encode native provider compaction terminal")?,
            self.physical_attempt_id.clone(),
            Uuid::new_v4().to_string(),
            Some(self.start_event_id.clone()),
            Some(self.last_causation_event_id.clone()),
            PhysicalAttemptAppendGuard::Terminal {
                entry,
                start_event_id: self.start_event_id.clone(),
                causation_event_id: self.last_causation_event_id.clone(),
            },
        )
        .await?;
        self.terminal_recorded = true;
        Ok(())
    }

    fn output_guard(&self) -> PhysicalAttemptAppendGuard {
        PhysicalAttemptAppendGuard::Output {
            physical_attempt_id: self.physical_attempt_id.clone(),
            start_event_id: self.start_event_id.clone(),
            causation_event_id: self.last_causation_event_id.clone(),
        }
    }

    fn record_output(&mut self, event_id: EventId) {
        self.last_causation_event_id = event_id.clone();
        self.durable_output_event_ids.push(event_id);
    }

    async fn persist_payload(
        &mut self,
        manifest: ProviderContinuationPayloadLifecycleEntry,
        opaque_payload: Vec<u8>,
    ) -> Result<()> {
        let coordinator = self
            .coordinator
            .take()
            .context("native provider compaction payload coordinator is unavailable")?;
        let joined = tokio::task::spawn_blocking(move || {
            let result = coordinator.persist_committed_payload(&manifest, &opaque_payload);
            (coordinator, result)
        })
        .await
        .context("native provider compaction payload task failed")?;
        self.coordinator = Some(joined.0);
        joined.1.map(|_| ())
    }
}

fn validate_covers_through(
    store: &JsonlSessionStore,
    session_scope_id: &str,
    covers_through: &CompactionCursor,
) -> Result<()> {
    if covers_through.session_id != session_scope_id {
        bail!("native provider compaction cursor belongs to a different session scope");
    }
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let exact_record = records.iter().find(|record| {
        let event = record.stored_event();
        event.session_id == session_scope_id
            && event.stream_sequence == covers_through.through_stream_sequence
            && event.event_id == covers_through.through_event_id
    });
    if exact_record.is_none() {
        bail!("native provider compaction cursor is not a durable session record");
    }
    Ok(())
}

async fn append_session_bound_direct_record_and_sync(
    store: JsonlSessionStore,
    session_scope_id: String,
    event_type: DurableEventType,
    payload: serde_json::Value,
    event_id: EventId,
    correlation_id: Option<EventId>,
    causation_id: Option<EventId>,
    guard: PhysicalAttemptAppendGuard,
) -> Result<()> {
    let reconciliation = DurableEventReconciliationExpectation::new_session_bound_direct(
        session_scope_id.clone(),
        event_type,
        event_id.clone(),
        payload.clone(),
        correlation_id.clone(),
        causation_id.clone(),
    )?;
    tokio::task::spawn_blocking(move || {
        match store.append_event_if_with_identity(
            event_type,
            payload,
            event_id,
            correlation_id,
            causation_id,
            |records| guard.validate(records),
        ) {
            Ok(Some(_)) => Ok(()),
            Ok(None) => bail!("native provider compaction durable append was not attempted"),
            Err(append_error) => match store.reconcile_durable_event(&reconciliation) {
                DurableEventReconciliation::ExactPresent(_) => Ok(()),
                DurableEventReconciliation::ConfirmedAbsent => Err(append_error
                    .context("native provider compaction durable append is confirmed absent")),
                DurableEventReconciliation::Conflict { reason } => Err(append_error.context(
                    format!("native provider compaction durable append conflicts: {reason}"),
                )),
                DurableEventReconciliation::Indeterminate { reason } => Err(append_error.context(
                    format!("native provider compaction durable append is indeterminate: {reason}"),
                )),
            },
        }
    })
    .await
    .context("native provider compaction durable append task failed")?
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
