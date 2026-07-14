use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};

use super::*;
use super::{
    compaction_v2::compaction_session_id,
    provider_continuation_payload_store::{
        ProviderContinuationPayloadPresence, ProviderContinuationPayloadStore,
        ProviderContinuationPayloadStoreGuard, ProviderContinuationSessionKeyStore,
        SystemProviderContinuationSessionKeyStore,
    },
};
use crate::EventId;

const ORPHAN_DISCOVERED_REASON: &str = "payload_missing_during_recovery";
const DELETED_AFTER_CLEANUP_REASON: &str = "payload_deleted_after_cleanup";
const CANDIDATE_INVALIDATED_RECOVERY_REASON: &str = "candidate_invalidated_recovery_cleanup";

/// Outcome of durably committing one encrypted provider continuation payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderContinuationPayloadCommitResult {
    /// The physical stage outcome before the durable manifest append.
    pub stage: ProviderContinuationPayloadStageResult,
    /// Whether this call appended the `Committed` manifest instead of replaying it.
    pub manifest_appended: bool,
    /// The physical finalize outcome after the durable manifest append.
    pub finalize: ProviderContinuationPayloadFinalizeResult,
}

/// Counts of the fail-closed local payload recovery actions completed in one pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderContinuationPayloadRecoveryReport {
    /// Staged ciphertext files removed because no durable committed manifest names them.
    pub discarded_uncommitted_stages: usize,
    /// Committed manifests whose staged ciphertext was atomically finalized.
    pub finalized: usize,
    /// Committed manifests durably marked orphaned because authenticated bytes were absent.
    pub orphaned: usize,
    /// Invalidated/orphaned manifests durably marked deleted after physical deletion.
    pub deleted: usize,
}

/// Outcome of invalidating and deleting one continuation payload through its durable lifecycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderContinuationPayloadRetentionResult {
    /// Whether this call appended the `Invalidated` transition.
    pub invalidated: bool,
    /// Whether this call appended the `Deleted` transition after physical deletion.
    pub deleted: bool,
}

/// Coordinates encrypted payload bytes with their append-only lifecycle records.
///
/// It owns the only supported write ordering for local continuation payloads:
/// `stage ciphertext -> append and sync Committed -> finalize`. Recovery and cleanup hold the
/// same cross-process payload lock while the relevant durable transition is decided, so a stale
/// recovery worker cannot mark a payload orphaned after another worker has finalized it.
pub struct ProviderContinuationPayloadCoordinator {
    inner: ProviderContinuationPayloadCoordinatorInner<SystemProviderContinuationSessionKeyStore>,
}

pub(crate) struct ProviderContinuationPayloadCoordinatorInner<K> {
    store: JsonlSessionStore,
    payload_store: ProviderContinuationPayloadStore<K>,
}

impl ProviderContinuationPayloadCoordinator {
    /// Opens the default encrypted continuation payload coordinator for one durable session.
    ///
    /// # Errors
    ///
    /// Returns an error when the durable session is invalid, its session scope cannot be bound to
    /// the local payload root, or the system credential store is unavailable.
    pub fn for_store(store: JsonlSessionStore) -> Result<Self> {
        let session_id = compaction_session_id(&store)?;
        let payload_store =
            ProviderContinuationPayloadStore::for_session_path(store.path(), session_id)?;
        Ok(Self {
            inner: ProviderContinuationPayloadCoordinatorInner {
                store,
                payload_store,
            },
        })
    }

    /// Stages, durably records, and finalizes one immutable continuation payload.
    ///
    /// # Errors
    ///
    /// Returns an error for missing secure-key access, malformed or unproven manifest provenance,
    /// integrity mismatch, append failure, or failed atomic finalization.
    pub fn persist_committed_payload(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
        payload: &[u8],
    ) -> Result<ProviderContinuationPayloadCommitResult> {
        self.inner.persist_committed_payload(manifest, payload)
    }

    /// Recovers unfinished stage/finalize/cleanup work without activating a candidate.
    ///
    /// # Errors
    ///
    /// Returns an error rather than guessing when the secure key, authenticated payload, durable
    /// lifecycle, or session scope cannot be proven.
    pub fn recover(&self) -> Result<ProviderContinuationPayloadRecoveryReport> {
        self.inner.recover()
    }

    /// Durably invalidates one payload and removes its encrypted local bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the lifecycle or authenticated local payload cannot be proven.
    pub fn invalidate_and_delete(
        &self,
        payload_id: &str,
        reason: impl Into<String>,
    ) -> Result<ProviderContinuationPayloadRetentionResult> {
        self.inner.invalidate_and_delete(payload_id, reason)
    }

    /// Durably cleans up one candidate-backed payload after its source-valid invalidation.
    ///
    /// # Errors
    ///
    /// Returns an error unless the candidate, observation, committed payload manifest, and
    /// durable invalidation terminal all belong to this session and agree exactly. It never
    /// deletes local ciphertext before the causally-linked `Invalidated` lifecycle record is
    /// durable.
    pub fn invalidate_candidate_backed_and_delete(
        &self,
        candidate_id: &str,
        reason: impl Into<String>,
    ) -> Result<ProviderContinuationPayloadRetentionResult> {
        self.inner
            .invalidate_candidate_backed_and_delete(candidate_id, reason)
    }
}

impl<K> ProviderContinuationPayloadCoordinatorInner<K>
where
    K: ProviderContinuationSessionKeyStore,
{
    /// Creates a coordinator around a test or platform-specific encrypted payload backend.
    #[cfg(test)]
    pub(crate) fn with_payload_store(
        store: JsonlSessionStore,
        payload_store: ProviderContinuationPayloadStore<K>,
    ) -> Self {
        Self {
            store,
            payload_store,
        }
    }

    /// Stages, durably records, and finalizes one immutable continuation payload.
    ///
    /// This operation sends no provider-native request and does not activate the candidate. A
    /// failed JSONL append leaves only encrypted staged bytes, which a later recovery pass may
    /// discard because no committed manifest exists.
    ///
    /// # Errors
    ///
    /// Returns an error for missing secure-key access, malformed or unproven manifest provenance,
    /// integrity mismatch, append failure, or failed atomic finalization.
    pub fn persist_committed_payload(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
        payload: &[u8],
    ) -> Result<ProviderContinuationPayloadCommitResult> {
        self.payload_store.with_locked_manifest_key_policy(
            manifest,
            || self.key_may_be_created(manifest),
            |guard| {
                let stage = guard.stage(payload)?;
                let manifest_appended = self
                    .append_committed_manifest_if_absent(manifest)?
                    .is_some();
                let finalize = guard.finalize()?;
                Ok(ProviderContinuationPayloadCommitResult {
                    stage,
                    manifest_appended,
                    finalize,
                })
            },
        )
    }

    fn key_may_be_created(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
    ) -> Result<bool> {
        let projection = self.payload_projection()?;
        match projection.payload(&manifest.payload_id) {
            None => Ok(true),
            Some(existing)
                if existing.manifest == *manifest
                    && existing.latest_lifecycle.state
                        == ProviderContinuationPayloadLifecycleState::Committed =>
            {
                Ok(false)
            }
            Some(_) => bail!(
                "provider continuation payload manifest already has a different durable lifecycle"
            ),
        }
    }

    /// Recovers unfinished stage/finalize/cleanup work without activating or materializing a
    /// provider-native candidate.
    ///
    /// # Errors
    ///
    /// Returns an error rather than guessing when the secure key, authenticated payload, durable
    /// lifecycle, or session scope cannot be proven.
    pub fn recover(&self) -> Result<ProviderContinuationPayloadRecoveryReport> {
        let projection = self.payload_projection()?;
        let payload_states = projection.payload_states().cloned().collect::<Vec<_>>();
        let committed_payload_ids = payload_states
            .iter()
            .filter(|state| {
                state.latest_lifecycle.state == ProviderContinuationPayloadLifecycleState::Committed
            })
            .map(|state| state.manifest.payload_id.clone())
            .collect::<BTreeSet<_>>();
        let mut report = ProviderContinuationPayloadRecoveryReport {
            discarded_uncommitted_stages: self
                .payload_store
                .discard_uncommitted_stages(&committed_payload_ids)?,
            ..ProviderContinuationPayloadRecoveryReport::default()
        };

        for state in payload_states {
            match state.latest_lifecycle.state {
                ProviderContinuationPayloadLifecycleState::Committed => {
                    if state.candidate_event_id.is_some()
                        && projection
                            .candidate_invalidation(&state.manifest.candidate_id)
                            .is_some()
                    {
                        let result = self.invalidate_candidate_backed_and_delete(
                            &state.manifest.candidate_id,
                            CANDIDATE_INVALIDATED_RECOVERY_REASON,
                        )?;
                        report.deleted += usize::from(result.deleted);
                    } else {
                        self.recover_committed_payload(&state, &mut report)?;
                    }
                }
                ProviderContinuationPayloadLifecycleState::Invalidated
                | ProviderContinuationPayloadLifecycleState::OrphanDiscovered => {
                    report.deleted += usize::from(self.finish_delete(&state)?);
                }
                ProviderContinuationPayloadLifecycleState::Deleted => {
                    self.assert_deleted_payload_absent(&state)?;
                }
            }
        }
        Ok(report)
    }

    /// Durably invalidates one payload and removes its encrypted local bytes.
    ///
    /// The `Invalidated` record is appended before physical deletion, while `Deleted` is appended
    /// only after the payload is absent. A crash between them remains a pinned cleanup obligation
    /// for [`Self::recover`].
    ///
    /// # Errors
    ///
    /// Returns an error when the payload cannot be proven to belong to this session, its key or
    /// bytes cannot be read safely, or either durable lifecycle transition cannot be appended.
    pub fn invalidate_and_delete(
        &self,
        payload_id: &str,
        reason: impl Into<String>,
    ) -> Result<ProviderContinuationPayloadRetentionResult> {
        let reason = reason.into();
        let initial = self.payload_state(payload_id)?;
        match initial.latest_lifecycle.state {
            ProviderContinuationPayloadLifecycleState::Deleted => {
                self.assert_deleted_payload_absent(&initial)?;
                return Ok(ProviderContinuationPayloadRetentionResult::default());
            }
            ProviderContinuationPayloadLifecycleState::Committed => {
                if initial.candidate_event_id.is_some() {
                    bail!(
                        "candidate-backed provider continuation payload requires a source-valid invalidation"
                    )
                }
            }
            ProviderContinuationPayloadLifecycleState::Invalidated
            | ProviderContinuationPayloadLifecycleState::OrphanDiscovered => {
                return Ok(ProviderContinuationPayloadRetentionResult {
                    invalidated: false,
                    deleted: self.finish_delete(&initial)?,
                });
            }
        }

        self.payload_store
            .with_locked_manifest(&initial.manifest, false, |guard| {
                let invalidated = lifecycle_transition(
                    &initial.manifest,
                    ProviderContinuationPayloadLifecycleState::Invalidated,
                    &reason,
                )?;
                let invalidated_appended = self
                    .append_transition_if_current(&initial, &invalidated, false)?
                    .is_some();
                let latest = self.payload_state(payload_id)?;
                if latest.latest_lifecycle.state
                    == ProviderContinuationPayloadLifecycleState::Deleted
                {
                    self.assert_guard_is_absent(guard)?;
                    return Ok(ProviderContinuationPayloadRetentionResult {
                        invalidated: invalidated_appended,
                        deleted: false,
                    });
                }
                let deleted = self.finish_delete_with_guard(guard, &latest)?;
                Ok(ProviderContinuationPayloadRetentionResult {
                    invalidated: invalidated_appended,
                    deleted,
                })
            })
    }

    /// Durably cleans up one candidate-backed payload after its source-valid invalidation.
    ///
    /// The terminal invalidation is the authority to start cleanup. This method reconstructs the
    /// exact candidate/observation/invalidation binding both before and while holding the payload
    /// lock, appends the causally-linked `Invalidated` record, then deletes encrypted bytes and
    /// records `Deleted`. A concurrent successful worker is treated as idempotent only when the
    /// same durable state can be reconstructed.
    pub fn invalidate_candidate_backed_and_delete(
        &self,
        candidate_id: &str,
        reason: impl Into<String>,
    ) -> Result<ProviderContinuationPayloadRetentionResult> {
        let reason = reason.into();
        let initial = self.candidate_cleanup_context(candidate_id)?;
        match initial.payload.latest_lifecycle.state {
            ProviderContinuationPayloadLifecycleState::Deleted => {
                self.assert_deleted_payload_absent(&initial.payload)?;
                return Ok(ProviderContinuationPayloadRetentionResult::default());
            }
            ProviderContinuationPayloadLifecycleState::OrphanDiscovered => {
                bail!("candidate-backed provider continuation payload cannot be orphaned")
            }
            ProviderContinuationPayloadLifecycleState::Committed
            | ProviderContinuationPayloadLifecycleState::Invalidated => {}
        }

        self.payload_store
            .with_locked_manifest(&initial.payload.manifest, false, |guard| {
                let current = self.candidate_cleanup_context(candidate_id)?;
                match current.payload.latest_lifecycle.state {
                    ProviderContinuationPayloadLifecycleState::Deleted => {
                        self.assert_guard_is_absent(guard)?;
                        Ok(ProviderContinuationPayloadRetentionResult::default())
                    }
                    ProviderContinuationPayloadLifecycleState::OrphanDiscovered => {
                        bail!("candidate-backed provider continuation payload cannot be orphaned")
                    }
                    ProviderContinuationPayloadLifecycleState::Invalidated => {
                        self.validate_candidate_invalidated_transition_links(&current)?;
                        Ok(ProviderContinuationPayloadRetentionResult {
                            invalidated: false,
                            deleted: self.finish_delete_with_guard(guard, &current.payload)?,
                        })
                    }
                    ProviderContinuationPayloadLifecycleState::Committed => {
                        let invalidated = lifecycle_transition(
                            &current.payload.manifest,
                            ProviderContinuationPayloadLifecycleState::Invalidated,
                            &reason,
                        )?;
                        let invalidated_appended = self
                            .append_candidate_invalidated_transition_if_current(
                                &current,
                                &invalidated,
                            )?
                            .is_some();
                        let latest = self.candidate_cleanup_context(candidate_id)?;
                        match latest.payload.latest_lifecycle.state {
                            ProviderContinuationPayloadLifecycleState::Deleted => {
                                self.assert_guard_is_absent(guard)?;
                                Ok(ProviderContinuationPayloadRetentionResult {
                                    invalidated: invalidated_appended,
                                    deleted: false,
                                })
                            }
                            ProviderContinuationPayloadLifecycleState::Invalidated => {
                                self.validate_candidate_invalidated_transition_links(&latest)?;
                                Ok(ProviderContinuationPayloadRetentionResult {
                                    invalidated: invalidated_appended,
                                    deleted: self.finish_delete_with_guard(guard, &latest.payload)?,
                                })
                            }
                            ProviderContinuationPayloadLifecycleState::Committed => bail!(
                                "candidate-backed provider continuation invalidation did not become durable"
                            ),
                            ProviderContinuationPayloadLifecycleState::OrphanDiscovered => bail!(
                                "candidate-backed provider continuation payload cannot be orphaned"
                            ),
                        }
                    }
                }
            })
    }

    fn recover_committed_payload(
        &self,
        state: &ProviderContinuationPayloadState,
        report: &mut ProviderContinuationPayloadRecoveryReport,
    ) -> Result<()> {
        self.payload_store
            .with_locked_manifest(&state.manifest, false, |guard| match guard.presence()? {
                ProviderContinuationPayloadPresence::Finalized => Ok(()),
                ProviderContinuationPayloadPresence::Staged => {
                    report.finalized += usize::from(matches!(
                        guard.finalize()?,
                        ProviderContinuationPayloadFinalizeResult::Finalized
                    ));
                    Ok(())
                }
                ProviderContinuationPayloadPresence::Missing => {
                    if state.candidate_event_id.is_some() {
                        bail!(
                            "recorded provider continuation candidate has no authenticated payload"
                        )
                    }
                    let orphan = lifecycle_transition(
                        &state.manifest,
                        ProviderContinuationPayloadLifecycleState::OrphanDiscovered,
                        ORPHAN_DISCOVERED_REASON,
                    )?;
                    report.orphaned += usize::from(
                        self.append_transition_if_current(state, &orphan, true)?
                            .is_some(),
                    );
                    Ok(())
                }
            })
    }

    fn finish_delete(&self, state: &ProviderContinuationPayloadState) -> Result<bool> {
        self.payload_store
            .with_locked_manifest(&state.manifest, false, |guard| {
                self.finish_delete_with_guard(guard, state)
            })
    }

    fn finish_delete_with_guard(
        &self,
        guard: &ProviderContinuationPayloadStoreGuard<'_, '_, K>,
        state: &ProviderContinuationPayloadState,
    ) -> Result<bool> {
        match state.latest_lifecycle.state {
            ProviderContinuationPayloadLifecycleState::Invalidated
            | ProviderContinuationPayloadLifecycleState::OrphanDiscovered => {}
            ProviderContinuationPayloadLifecycleState::Deleted => {
                self.assert_guard_is_absent(guard)?;
                return Ok(false);
            }
            ProviderContinuationPayloadLifecycleState::Committed => {
                bail!("provider continuation payload must be invalidated before deletion")
            }
        }
        let _ = guard.delete()?;
        let deleted = lifecycle_transition(
            &state.manifest,
            ProviderContinuationPayloadLifecycleState::Deleted,
            DELETED_AFTER_CLEANUP_REASON,
        )?;
        Ok(self
            .append_transition_if_current(state, &deleted, false)?
            .is_some())
    }

    fn assert_deleted_payload_absent(
        &self,
        state: &ProviderContinuationPayloadState,
    ) -> Result<()> {
        self.payload_store
            .with_locked_manifest(&state.manifest, false, |guard| {
                self.assert_guard_is_absent(guard)
            })
    }

    fn assert_guard_is_absent(
        &self,
        guard: &ProviderContinuationPayloadStoreGuard<'_, '_, K>,
    ) -> Result<()> {
        if guard.presence()? != ProviderContinuationPayloadPresence::Missing {
            bail!("deleted provider continuation payload still has local ciphertext")
        }
        Ok(())
    }

    fn append_committed_manifest_if_absent(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
    ) -> Result<Option<StoredEvent>> {
        let event_id = provider_continuation_payload_lifecycle_event_id(
            &manifest.payload_id,
            ProviderContinuationPayloadLifecycleState::Committed,
        );
        let payload = serde_json::to_value(manifest)
            .context("failed to encode provider continuation committed manifest")?;
        self.store.append_event_if_with_identity(
            DurableEventType::ProviderContinuationPayloadLifecycleRecorded,
            payload,
            event_id,
            None,
            None,
            |records| {
                self.ensure_store_session_scope(records)?;
                let projection = ProviderContinuationProjection::from_records(records)?;
                if let Some(existing) = projection.payload(&manifest.payload_id) {
                    if existing.manifest == *manifest
                        && existing.latest_lifecycle.state
                            == ProviderContinuationPayloadLifecycleState::Committed
                    {
                        return Ok(false);
                    }
                    bail!(
                        "provider continuation payload manifest already has a different lifecycle"
                    )
                }
                let session_id = stream_session_id(records)
                    .unwrap_or_else(|| session_id_for_path(self.store.path()));
                let mut prospective = records.to_vec();
                prospective.push(SessionStreamRecord::Stored(StoredEvent::new(
                    DurableEventType::ProviderContinuationPayloadLifecycleRecorded,
                    EventClass::Critical,
                    provider_continuation_payload_lifecycle_event_id(
                        &manifest.payload_id,
                        ProviderContinuationPayloadLifecycleState::Committed,
                    ),
                    session_id,
                    records.len() as u64 + 1,
                    serde_json::to_value(manifest)
                        .context("failed to encode prospective continuation manifest")?,
                )?));
                ProviderContinuationProjection::from_records(&prospective)?;
                Ok(true)
            },
        )
    }

    fn append_transition_if_current(
        &self,
        expected: &ProviderContinuationPayloadState,
        transition: &ProviderContinuationPayloadLifecycleEntry,
        require_no_candidate: bool,
    ) -> Result<Option<StoredEvent>> {
        let payload = serde_json::to_value(transition)
            .context("failed to encode provider continuation payload lifecycle transition")?;
        self.store.append_event_if_with_identity(
            DurableEventType::ProviderContinuationPayloadLifecycleRecorded,
            payload,
            provider_continuation_payload_lifecycle_event_id(
                &transition.payload_id,
                transition.state,
            ),
            None,
            None,
            |records| {
                self.ensure_store_session_scope(records)?;
                let projection = ProviderContinuationProjection::from_records(records)?;
                let current = projection.payload(&expected.manifest.payload_id).with_context(|| {
                    format!(
                        "provider continuation payload {} disappeared during lifecycle transition",
                        expected.manifest.payload_id
                    )
                })?;
                if current.manifest != expected.manifest {
                    bail!(
                        "provider continuation payload manifest changed during lifecycle transition"
                    )
                }
                if require_no_candidate && current.candidate_event_id.is_some() {
                    bail!("recorded provider continuation candidate cannot be marked orphaned")
                }
                if current.latest_event_id == expected.latest_event_id
                    && current.latest_lifecycle == expected.latest_lifecycle
                {
                    return Ok(true);
                }
                if current.latest_lifecycle == *transition {
                    return Ok(false);
                }
                bail!("provider continuation payload lifecycle changed during transition")
            },
        )
    }

    fn append_candidate_invalidated_transition_if_current(
        &self,
        expected: &CandidatePayloadCleanupContext,
        transition: &ProviderContinuationPayloadLifecycleEntry,
    ) -> Result<Option<StoredEvent>> {
        let payload = serde_json::to_value(transition).context(
            "failed to encode candidate-backed provider continuation invalidation transition",
        )?;
        let event_id = provider_continuation_payload_lifecycle_event_id(
            &transition.payload_id,
            ProviderContinuationPayloadLifecycleState::Invalidated,
        );
        let expected_context = expected.clone();
        let expected_transition = transition.clone();
        self.store.append_event_if_with_identity(
            DurableEventType::ProviderContinuationPayloadLifecycleRecorded,
            payload,
            event_id,
            Some(expected.observation_correlation_id.clone()),
            Some(expected.invalidation.event_id.clone()),
            move |records| {
                self.ensure_store_session_scope(records)?;
                let projection = ProviderContinuationProjection::from_records(records)?;
                let current = candidate_cleanup_context_from_projection(
                    &projection,
                    &expected_context.candidate_id,
                )?;
                if current != expected_context {
                    bail!("candidate-backed provider continuation cleanup frontier drifted")
                }
                if current.payload.latest_lifecycle != expected_context.payload.latest_lifecycle
                    || current.payload.latest_event_id != expected_context.payload.latest_event_id
                {
                    bail!("candidate-backed provider continuation payload lifecycle changed")
                }
                let mut prospective = records.to_vec();
                let mut event = StoredEvent::new(
                    DurableEventType::ProviderContinuationPayloadLifecycleRecorded,
                    EventClass::Critical,
                    provider_continuation_payload_lifecycle_event_id(
                        &expected_transition.payload_id,
                        ProviderContinuationPayloadLifecycleState::Invalidated,
                    ),
                    current.session_id.clone(),
                    next_stream_sequence(records),
                    serde_json::to_value(&expected_transition).context(
                        "failed to encode prospective candidate-backed invalidation transition",
                    )?,
                )?;
                event.correlation_id = Some(current.observation_correlation_id.clone());
                event.causation_id = Some(current.invalidation.event_id.clone());
                event.record_checksum = event.compute_record_checksum()?;
                prospective.push(SessionStreamRecord::Stored(event));
                ProviderContinuationProjection::from_records(&prospective)?;
                Ok(true)
            },
        )
    }

    fn candidate_cleanup_context(
        &self,
        candidate_id: &str,
    ) -> Result<CandidatePayloadCleanupContext> {
        let projection = self.payload_projection()?;
        candidate_cleanup_context_from_projection(&projection, candidate_id)
    }

    fn validate_candidate_invalidated_transition_links(
        &self,
        context: &CandidatePayloadCleanupContext,
    ) -> Result<()> {
        let records = self.store.read_event_records_writer()?;
        self.ensure_store_session_scope(&records)?;
        let event = records
            .iter()
            .map(SessionStreamRecord::stored_event)
            .find(|event| event.event_id == context.payload.latest_event_id)
            .context("candidate-backed provider continuation invalidation event is missing")?;
        if event.correlation_id.as_deref() != Some(context.observation_correlation_id.as_str()) {
            bail!("candidate-backed provider continuation invalidation correlation drifted")
        }
        if event.causation_id.as_deref() != Some(context.invalidation.event_id.as_str()) {
            bail!("candidate-backed provider continuation invalidation causation drifted")
        }
        Ok(())
    }

    fn payload_projection(&self) -> Result<ProviderContinuationProjection> {
        let records = self.store.read_event_records_writer()?;
        self.ensure_store_session_scope(&records)?;
        ProviderContinuationProjection::from_records(&records)
    }

    fn payload_state(&self, payload_id: &str) -> Result<ProviderContinuationPayloadState> {
        self.payload_projection()?
            .payload(payload_id)
            .cloned()
            .with_context(|| format!("provider continuation payload {payload_id} is missing"))
    }

    fn ensure_store_session_scope(&self, records: &[SessionStreamRecord]) -> Result<()> {
        let session_id =
            stream_session_id(records).unwrap_or_else(|| session_id_for_path(self.store.path()));
        if session_id != self.payload_store.session_id() {
            bail!("provider continuation payload store belongs to a different session scope")
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CandidatePayloadCleanupContext {
    candidate_id: String,
    session_id: String,
    observation_correlation_id: EventId,
    invalidation: ProviderContinuationCandidateInvalidationState,
    payload: ProviderContinuationPayloadState,
}

fn candidate_cleanup_context_from_projection(
    projection: &ProviderContinuationProjection,
    candidate_id: &str,
) -> Result<CandidatePayloadCleanupContext> {
    let candidate = projection.candidate(candidate_id).with_context(|| {
        format!("provider continuation candidate {candidate_id} is missing for cleanup")
    })?;
    let observation_id = candidate.entry.observation_id.as_deref().context(
        "initiated provider continuation candidate cannot use candidate invalidation cleanup",
    )?;
    let observation = projection.observation(observation_id).with_context(|| {
        format!(
            "provider continuation candidate {candidate_id} has no durable provider observation"
        )
    })?;
    let invalidation = projection
        .candidate_invalidation(candidate_id)
        .with_context(|| {
            format!(
                "provider continuation candidate {candidate_id} has no source-valid invalidation"
            )
        })?;
    if invalidation.entry.candidate_id != candidate.entry.candidate_id
        || invalidation.entry.observation_id != observation.entry.observation_id
        || invalidation.entry.source_event_id != candidate.event_id
        || invalidation.session_id != candidate.session_id
        || observation.session_id != candidate.session_id
    {
        bail!("candidate-backed provider continuation cleanup provenance drifted")
    }
    let payload_id = &candidate.entry.candidate.payload().payload_id;
    let payload = projection.payload(payload_id).cloned().with_context(|| {
        format!("provider continuation candidate {candidate_id} has no committed payload manifest")
    })?;
    if payload.manifest.candidate_id != candidate.entry.candidate_id
        || payload.candidate_event_id.as_deref() != Some(candidate.event_id.as_str())
    {
        bail!("candidate-backed provider continuation cleanup payload binding drifted")
    }
    Ok(CandidatePayloadCleanupContext {
        candidate_id: candidate.entry.candidate_id.clone(),
        session_id: candidate.session_id.clone(),
        observation_correlation_id: observation.correlation_id.clone(),
        invalidation: invalidation.clone(),
        payload,
    })
}

fn lifecycle_transition(
    manifest: &ProviderContinuationPayloadLifecycleEntry,
    state: ProviderContinuationPayloadLifecycleState,
    reason: &str,
) -> Result<ProviderContinuationPayloadLifecycleEntry> {
    let mut transition = manifest.clone();
    transition.state = state;
    transition.reason = Some(reason.to_owned());
    transition.validate_shape()?;
    Ok(transition)
}

#[cfg(test)]
#[path = "tests/provider_continuation_payload_coordinator_tests.rs"]
mod tests;
