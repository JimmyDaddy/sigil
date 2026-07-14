use std::fs;

use crate::VersionedProfileIdentity;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::super::provider_continuation_payload_store::{
    InMemoryProviderContinuationSessionKeyStore, ProviderContinuationPayloadStore,
};
use super::*;

const ATTEMPT_ID: &str = "native-continuation-attempt";
const OBSERVED_ATTEMPT_ID: &str = "native-continuation-observed-attempt";
const OBSERVED_START_EVENT_ID: &str = "event-native-observed-start";

fn hmac(byte: char) -> String {
    format!("hmac-sha256:{}", byte.to_string().repeat(64))
}

fn profile(id: &str) -> VersionedProfileIdentity {
    VersionedProfileIdentity::from_content(id, 1, id.as_bytes())
}

fn append_started_manifest(
    store: &JsonlSessionStore,
    payload: &[u8],
) -> Result<ProviderContinuationPayloadLifecycleEntry> {
    let started = store.append_compaction_started(CompactionStartedEntry {
        attempt_id: ATTEMPT_ID.to_owned(),
        fallback_parent: CompactionFallbackParent::Root,
        initiation: CompactionInitiation::Manual,
        base_projection_revision: "continuation-payload-test-r1".to_owned(),
        started_at_unix_ms: 1,
    })?;
    let session_id = JsonlSessionStore::read_event_records(store.path())?
        .first()
        .context("compaction start should have a durable session id")?
        .session_id()
        .to_owned();
    let candidate_id = provider_continuation_candidate_id_from_initiated(
        &session_id,
        &started.event_id,
        ATTEMPT_ID,
    );
    Ok(ProviderContinuationPayloadLifecycleEntry {
        schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
        payload_id: provider_continuation_payload_id(
            &candidate_id,
            ProviderContinuationPayloadKind::Artifact,
        ),
        candidate_id,
        source: ProviderContinuationPayloadSource::Initiated {
            started_event_id: started.event_id,
            attempt_id: ATTEMPT_ID.to_owned(),
        },
        kind: ProviderContinuationPayloadKind::Artifact,
        storage_ref: ProviderContinuationPayloadStorageRef::Artifact {
            artifact_id: "artifact-native-continuation".to_owned(),
        },
        integrity: ProviderContinuationPayloadIntegrity::Sha256(format!(
            "sha256:{:x}",
            Sha256::digest(payload)
        )),
        byte_size: payload.len() as u64,
        state: ProviderContinuationPayloadLifecycleState::Committed,
        reason: None,
    })
}

fn payload_store(
    root: &std::path::Path,
    session_id: &str,
) -> Result<ProviderContinuationPayloadStore<InMemoryProviderContinuationSessionKeyStore>> {
    ProviderContinuationPayloadStore::new(
        root,
        session_id,
        InMemoryProviderContinuationSessionKeyStore::default(),
    )
}

fn store_session_id(store: &JsonlSessionStore) -> Result<String> {
    Ok(JsonlSessionStore::read_event_records(store.path())?
        .first()
        .context("fixture session should have one durable record")?
        .session_id()
        .to_owned())
}

fn append_committed_manifest(
    store: &JsonlSessionStore,
    manifest: &ProviderContinuationPayloadLifecycleEntry,
) -> Result<()> {
    store.append_event_if_with_identity(
        DurableEventType::ProviderContinuationPayloadLifecycleRecorded,
        serde_json::to_value(manifest)?,
        provider_continuation_payload_lifecycle_event_id(
            &manifest.payload_id,
            ProviderContinuationPayloadLifecycleState::Committed,
        ),
        None,
        None,
        |_| Ok(true),
    )?;
    Ok(())
}

fn append_candidate_backed_payload(
    store: &JsonlSessionStore,
    coordinator: &ProviderContinuationPayloadCoordinatorInner<
        InMemoryProviderContinuationSessionKeyStore,
    >,
    payload: &[u8],
) -> Result<(
    ProviderContinuationObservedEntry,
    ProviderContinuationCandidateRecordedEntry,
    ProviderContinuationPayloadLifecycleEntry,
)> {
    let session_id = store_session_id(store)?;
    let observed_payload_integrity_tag = hmac('b');
    let observed = ProviderContinuationObservedEntry {
        schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
        observation_id: provider_continuation_observation_id(
            &session_id,
            &hmac('c'),
            OBSERVED_ATTEMPT_ID,
            0,
            &observed_payload_integrity_tag,
        ),
        physical_attempt_id: OBSERVED_ATTEMPT_ID.to_owned(),
        response_item_ordinal: 0,
        observed_payload_integrity_tag,
        provider_name: "test-provider".to_owned(),
        provider_route_fingerprint: hmac('c'),
        model_name: "test-model".to_owned(),
        model_metadata_profile: profile("model-metadata"),
        wire_profile: profile("wire"),
        wire_protocol: "test-wire".to_owned(),
        wire_schema_version: "v1".to_owned(),
        provider_request_id: Some("request-1".to_owned()),
        provider_response_id: Some("response-1".to_owned()),
        observed_at_unix_ms: 2,
    };
    let observed_event_id = provider_continuation_observed_event_id(&observed.observation_id);
    store
        .append_event_if_with_identity(
            DurableEventType::ProviderContinuationObserved,
            serde_json::to_value(&observed)?,
            observed_event_id.clone(),
            Some(OBSERVED_START_EVENT_ID.to_owned()),
            Some(OBSERVED_START_EVENT_ID.to_owned()),
            |_| Ok(true),
        )?
        .expect("provider observation should append");

    let candidate_id =
        provider_continuation_candidate_id_from_observation(&observed.observation_id);
    let payload_id =
        provider_continuation_payload_id(&candidate_id, ProviderContinuationPayloadKind::Artifact);
    let integrity = ProviderContinuationPayloadIntegrity::Sha256(format!(
        "sha256:{:x}",
        Sha256::digest(payload)
    ));
    let candidate = ProviderContinuationCandidateRecordedEntry {
        schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
        candidate_id: candidate_id.clone(),
        observation_id: Some(observed.observation_id.clone()),
        candidate: ProviderContinuationCandidate::Artifact(ProviderCompactionArtifactRef {
            candidate_id: candidate_id.clone(),
            payload: ProviderContinuationPayloadIdentity {
                payload_id: payload_id.clone(),
                integrity: integrity.clone(),
                byte_size: payload.len() as u64,
            },
            artifact_id: "artifact-observed-native-continuation".to_owned(),
            provider_name: "test-provider".to_owned(),
            provider_route_fingerprint: hmac('c'),
            model_name: "test-model".to_owned(),
            model_metadata_profile: profile("model-metadata"),
            wire_profile: profile("wire"),
            wire_protocol: "test-wire".to_owned(),
            wire_schema_version: "v1".to_owned(),
            composition_profile: profile("composition"),
            artifact_kind: "compaction-result".to_owned(),
            composition_mode: ProviderArtifactComposition::ReplacementWindow,
            covers_through: CompactionCursor {
                session_id,
                through_stream_sequence: 1,
                through_event_id: OBSERVED_START_EVENT_ID.to_owned(),
            },
            request_fingerprint: hmac('e'),
            sensitivity: ContextSensitivity::Repository,
        }),
        resolution_mode: ProviderContinuationResolutionMode::NativeOnly,
        activation_gate: ProviderContinuationActivationGate::Immediate,
        source_event_id: observed_event_id.clone(),
        created_at_unix_ms: 3,
    };
    let manifest = ProviderContinuationPayloadLifecycleEntry {
        schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
        payload_id,
        candidate_id: candidate_id.clone(),
        source: ProviderContinuationPayloadSource::ProviderObserved {
            observation_event_id: observed_event_id.clone(),
            observation_id: observed.observation_id.clone(),
        },
        kind: ProviderContinuationPayloadKind::Artifact,
        storage_ref: ProviderContinuationPayloadStorageRef::Artifact {
            artifact_id: "artifact-observed-native-continuation".to_owned(),
        },
        integrity,
        byte_size: payload.len() as u64,
        state: ProviderContinuationPayloadLifecycleState::Committed,
        reason: None,
    };
    coordinator.persist_committed_payload(&manifest, payload)?;

    let candidate_event_id = provider_continuation_candidate_recorded_event_id(&candidate_id);
    store
        .append_event_if_with_identity(
            DurableEventType::ProviderContinuationCandidateRecorded,
            serde_json::to_value(&candidate)?,
            candidate_event_id.clone(),
            Some(OBSERVED_START_EVENT_ID.to_owned()),
            Some(observed_event_id.clone()),
            |_| Ok(true),
        )?
        .expect("provider candidate should append");
    store
        .append_event_if_with_identity(
            DurableEventType::ProviderPhysicalAttemptTerminal,
            serde_json::to_value(ProviderPhysicalAttemptTerminalEntry {
                schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
                physical_attempt_id: OBSERVED_ATTEMPT_ID.to_owned(),
                request_material_fingerprint: hmac('a'),
                outcome: ProviderPhysicalAttemptOutcome::Completed,
                rejection: None,
                provider_request_id: None,
                provider_response_id: None,
                durable_output_event_ids: vec![observed_event_id, candidate_event_id.clone()],
                durable_side_effect_event_ids: Vec::new(),
                finished_at_unix_ms: 5,
            })?,
            "event-native-observed-terminal".to_owned(),
            Some(OBSERVED_START_EVENT_ID.to_owned()),
            Some(candidate_event_id),
            |_| Ok(true),
        )?
        .expect("provider terminal should append");
    Ok((observed, candidate, manifest))
}

fn append_observed_physical_start(store: &JsonlSessionStore) -> Result<()> {
    let started = ProviderPhysicalAttemptStartedEntry {
        schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
        physical_attempt_id: OBSERVED_ATTEMPT_ID.to_owned(),
        logical_run_id: "native-continuation-observed-run".to_owned(),
        purpose: ProviderPhysicalAttemptPurpose::NativeCompaction,
        request_material_fingerprint: hmac('a'),
        provider_name: "test-provider".to_owned(),
        model_name: "test-model".to_owned(),
        started_at_unix_ms: 1,
    };
    store
        .append_event_if_with_identity(
            DurableEventType::ProviderPhysicalAttemptStarted,
            serde_json::to_value(started)?,
            OBSERVED_START_EVENT_ID.to_owned(),
            Some(OBSERVED_START_EVENT_ID.to_owned()),
            None,
            |_| Ok(true),
        )?
        .expect("physical attempt start should append");
    Ok(())
}

fn append_candidate_invalidation(
    store: JsonlSessionStore,
    observed: &ProviderContinuationObservedEntry,
    candidate: &ProviderContinuationCandidateRecordedEntry,
) -> Result<()> {
    let entry = ProviderContinuationCandidateInvalidatedEntry {
        schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
        candidate_id: candidate.candidate_id.clone(),
        observation_id: observed.observation_id.clone(),
        source_event_id: provider_continuation_candidate_recorded_event_id(&candidate.candidate_id),
        basis: ProviderContinuationCandidateInvalidationBasis::SourceOnly,
        reason: ProviderContinuationCandidateInvalidationReason::FrozenEvidenceRejected,
        invalidated_at_unix_ms: 10,
    };
    let result = ProviderContinuationCandidateInvalidationCoordinator::new(store)
        .append_or_reconcile(entry)?;
    assert!(matches!(
        result,
        ProviderContinuationCandidateInvalidationPersistence::Recorded { .. }
    ));
    Ok(())
}

fn staged_payload_path(root: &std::path::Path) -> Result<std::path::PathBuf> {
    fs::read_dir(root)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "stage")
        })
        .context("expected one encrypted staged payload")
}

#[test]
fn coordinator_stages_records_and_finalizes_without_payload_bytes_in_jsonl() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let payload = b"opaque-native-continuation-artifact";
    let manifest = append_started_manifest(&store, payload)?;
    let root = temp.path().join("payloads");
    let backend = payload_store(&root, &store_session_id(&store)?)?;
    let coordinator =
        ProviderContinuationPayloadCoordinatorInner::with_payload_store(store.clone(), backend);

    let result = coordinator.persist_committed_payload(&manifest, payload)?;

    assert!(result.manifest_appended);
    assert_eq!(
        result.finalize,
        ProviderContinuationPayloadFinalizeResult::Finalized
    );
    let jsonl = fs::read(store.path())?;
    assert!(!jsonl.windows(payload.len()).any(|window| window == payload));
    assert!(fs::read_dir(&root)?.any(|entry| {
        entry
            .expect("payload directory entry should be readable")
            .path()
            .extension()
            .is_some_and(|extension| extension == "payload")
    }));
    assert_eq!(
        store
            .provider_continuation_projection()?
            .payload(&manifest.payload_id)
            .expect("committed manifest should project")
            .latest_lifecycle
            .state,
        ProviderContinuationPayloadLifecycleState::Committed
    );
    Ok(())
}

#[test]
fn recovery_discards_ciphertext_staged_before_any_durable_manifest() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let payload = b"stage-before-manifest";
    let manifest = append_started_manifest(&store, payload)?;
    let root = temp.path().join("payloads");
    let backend = payload_store(&root, &store_session_id(&store)?)?;
    backend.stage(&manifest, payload)?;
    assert!(staged_payload_path(&root)?.exists());
    let coordinator =
        ProviderContinuationPayloadCoordinatorInner::with_payload_store(store, backend);

    let report = coordinator.recover()?;

    assert_eq!(report.discarded_uncommitted_stages, 1);
    assert!(staged_payload_path(&root).is_err());
    Ok(())
}

#[test]
fn recovery_finalizes_staged_ciphertext_after_the_manifest_is_durable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let payload = b"manifest-before-finalize";
    let manifest = append_started_manifest(&store, payload)?;
    let root = temp.path().join("payloads");
    let backend = payload_store(&root, &store_session_id(&store)?)?;
    backend.stage(&manifest, payload)?;
    append_committed_manifest(&store, &manifest)?;
    let coordinator =
        ProviderContinuationPayloadCoordinatorInner::with_payload_store(store.clone(), backend);

    let report = coordinator.recover()?;

    assert_eq!(report.finalized, 1);
    assert!(staged_payload_path(&root).is_err());
    assert!(fs::read_dir(&root)?.any(|entry| {
        entry
            .expect("payload directory entry should be readable")
            .path()
            .extension()
            .is_some_and(|extension| extension == "payload")
    }));
    assert_eq!(
        store
            .provider_continuation_projection()?
            .payload(&manifest.payload_id)
            .expect("manifest should remain committed")
            .latest_lifecycle
            .state,
        ProviderContinuationPayloadLifecycleState::Committed
    );
    Ok(())
}

#[test]
fn recovery_with_a_missing_session_key_fails_closed_without_lifecycle_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let payload = b"key-must-not-be-regenerated-during-recovery";
    let manifest = append_started_manifest(&store, payload)?;
    let root = temp.path().join("payloads");
    let session_id = store_session_id(&store)?;
    let keyed_backend = payload_store(&root, &session_id)?;
    keyed_backend.stage(&manifest, payload)?;
    append_committed_manifest(&store, &manifest)?;
    let before = fs::read(store.path())?;
    drop(keyed_backend);
    let missing_key_backend = payload_store(&root, &session_id)?;
    let coordinator = ProviderContinuationPayloadCoordinatorInner::with_payload_store(
        store.clone(),
        missing_key_backend,
    );

    let error = coordinator
        .recover()
        .expect_err("recovery must not create a replacement session key");

    assert!(error.to_string().contains("session key is unavailable"));
    assert_eq!(fs::read(store.path())?, before);
    assert!(staged_payload_path(&root)?.exists());
    Ok(())
}

#[test]
fn persisted_manifest_with_a_missing_session_key_never_creates_a_replacement() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let payload = b"key-must-not-be-regenerated-during-persist";
    let manifest = append_started_manifest(&store, payload)?;
    let root = temp.path().join("payloads");
    let session_id = store_session_id(&store)?;
    let keyed_backend = payload_store(&root, &session_id)?;
    keyed_backend.stage(&manifest, payload)?;
    append_committed_manifest(&store, &manifest)?;
    let before = fs::read(store.path())?;
    drop(keyed_backend);

    let missing_key_backend = payload_store(&root, &session_id)?;
    let coordinator = ProviderContinuationPayloadCoordinatorInner::with_payload_store(
        store.clone(),
        missing_key_backend,
    );
    let error = coordinator
        .persist_committed_payload(&manifest, payload)
        .expect_err("an existing durable payload manifest must never mint a replacement key");

    assert!(error.to_string().contains("session key is unavailable"));
    assert_eq!(fs::read(store.path())?, before);
    assert!(staged_payload_path(&root)?.exists());
    Ok(())
}

#[test]
fn recovery_marks_missing_authenticated_payload_orphan_then_deleted() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let payload = b"lost-after-manifest";
    let manifest = append_started_manifest(&store, payload)?;
    let root = temp.path().join("payloads");
    let backend = payload_store(&root, &store_session_id(&store)?)?;
    backend.stage(&manifest, payload)?;
    fs::remove_file(staged_payload_path(&root)?)?;
    append_committed_manifest(&store, &manifest)?;
    let coordinator =
        ProviderContinuationPayloadCoordinatorInner::with_payload_store(store.clone(), backend);

    let orphan_report = coordinator.recover()?;

    assert_eq!(orphan_report.orphaned, 1);
    assert_eq!(
        store
            .provider_continuation_projection()?
            .payload(&manifest.payload_id)
            .expect("orphaned payload should project")
            .latest_lifecycle
            .state,
        ProviderContinuationPayloadLifecycleState::OrphanDiscovered
    );
    let deletion_report = coordinator.recover()?;
    assert_eq!(deletion_report.deleted, 1);
    assert_eq!(
        store
            .provider_continuation_projection()?
            .payload(&manifest.payload_id)
            .expect("deleted payload should remain auditable")
            .latest_lifecycle
            .state,
        ProviderContinuationPayloadLifecycleState::Deleted
    );
    Ok(())
}

#[test]
fn invalidation_is_durable_before_the_payload_is_deleted() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let payload = b"retain-then-invalidate";
    let manifest = append_started_manifest(&store, payload)?;
    let root = temp.path().join("payloads");
    let backend = payload_store(&root, &store_session_id(&store)?)?;
    let coordinator =
        ProviderContinuationPayloadCoordinatorInner::with_payload_store(store.clone(), backend);
    coordinator.persist_committed_payload(&manifest, payload)?;

    let result = coordinator.invalidate_and_delete(&manifest.payload_id, "superseded")?;

    assert!(result.invalidated);
    assert!(result.deleted);
    assert!(!fs::read_dir(&root)?.any(|entry| {
        entry
            .expect("payload directory entry should be readable")
            .path()
            .extension()
            .is_some_and(|extension| extension == "payload" || extension == "stage")
    }));
    let projection = store.provider_continuation_projection()?;
    let state = projection
        .payload(&manifest.payload_id)
        .expect("deleted payload should remain projected");
    assert_eq!(
        state.latest_lifecycle.state,
        ProviderContinuationPayloadLifecycleState::Deleted
    );
    assert!(projection.retention_pins().is_empty());
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let lifecycle_states = records
        .iter()
        .filter(|record| {
            record.stored_event().event_type
                == DurableEventType::ProviderContinuationPayloadLifecycleRecorded.as_str()
        })
        .map(|record| {
            record
                .stored_event()
                .payload
                .get("state")
                .and_then(serde_json::Value::as_str)
                .expect("lifecycle state should be serialized")
        })
        .collect::<Vec<_>>();
    assert_eq!(lifecycle_states, ["committed", "invalidated", "deleted"]);
    Ok(())
}

#[test]
fn candidate_backed_cleanup_requires_a_durable_invalidation_then_links_the_lifecycle() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let payload = b"candidate-backed-retention";
    let root = temp.path().join("payloads");
    append_observed_physical_start(&store)?;
    let backend = payload_store(&root, &store_session_id(&store)?)?;
    let coordinator =
        ProviderContinuationPayloadCoordinatorInner::with_payload_store(store.clone(), backend);
    let (observed, candidate, manifest) =
        append_candidate_backed_payload(&store, &coordinator, payload)?;

    let generic_error = coordinator
        .invalidate_and_delete(&manifest.payload_id, "superseded")
        .expect_err("generic cleanup must not bypass candidate invalidation");
    assert!(
        generic_error
            .to_string()
            .contains("requires a source-valid invalidation")
    );
    let cleanup_error = coordinator
        .invalidate_candidate_backed_and_delete(&candidate.candidate_id, "superseded")
        .expect_err("candidate cleanup requires a durable invalidation terminal");
    assert!(
        cleanup_error
            .to_string()
            .contains("has no source-valid invalidation")
    );
    assert!(fs::read_dir(&root)?.any(|entry| {
        entry
            .expect("payload directory entry should be readable")
            .path()
            .extension()
            .is_some_and(|extension| extension == "payload")
    }));

    append_candidate_invalidation(store.clone(), &observed, &candidate)?;
    let result = coordinator
        .invalidate_candidate_backed_and_delete(&candidate.candidate_id, "superseded")?;

    assert_eq!(
        result,
        ProviderContinuationPayloadRetentionResult {
            invalidated: true,
            deleted: true,
        }
    );
    let projection = store.provider_continuation_projection()?;
    assert_eq!(
        projection
            .payload(&manifest.payload_id)
            .expect("candidate payload should remain auditable")
            .latest_lifecycle
            .state,
        ProviderContinuationPayloadLifecycleState::Deleted
    );
    assert!(projection.retention_pins().is_empty());
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let invalidated = records
        .iter()
        .map(SessionStreamRecord::stored_event)
        .find(|event| {
            event.event_type
                == DurableEventType::ProviderContinuationPayloadLifecycleRecorded.as_str()
                && event
                    .payload
                    .get("state")
                    .and_then(serde_json::Value::as_str)
                    == Some("invalidated")
        })
        .context("candidate payload invalidation lifecycle should be durable")?;
    assert_eq!(
        invalidated.correlation_id.as_deref(),
        Some(OBSERVED_START_EVENT_ID)
    );
    let invalidation_event_id =
        provider_continuation_candidate_invalidated_event_id(&candidate.candidate_id);
    assert_eq!(
        invalidated.causation_id.as_deref(),
        Some(invalidation_event_id.as_str())
    );
    assert!(!fs::read_dir(&root)?.any(|entry| {
        entry
            .expect("payload directory entry should be readable")
            .path()
            .extension()
            .is_some_and(|extension| extension == "payload" || extension == "stage")
    }));
    Ok(())
}

#[test]
fn recovery_continues_candidate_cleanup_after_the_invalidation_terminal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let payload = b"candidate-backed-recovery";
    let root = temp.path().join("payloads");
    append_observed_physical_start(&store)?;
    let backend = payload_store(&root, &store_session_id(&store)?)?;
    let coordinator =
        ProviderContinuationPayloadCoordinatorInner::with_payload_store(store.clone(), backend);
    let (observed, candidate, manifest) =
        append_candidate_backed_payload(&store, &coordinator, payload)?;
    append_candidate_invalidation(store.clone(), &observed, &candidate)?;

    let report = coordinator.recover()?;

    assert_eq!(report.deleted, 1);
    let projection = store.provider_continuation_projection()?;
    assert_eq!(
        projection
            .payload(&manifest.payload_id)
            .expect("candidate payload should remain auditable")
            .latest_lifecycle
            .state,
        ProviderContinuationPayloadLifecycleState::Deleted
    );
    assert!(projection.retention_pins().is_empty());
    assert!(!fs::read_dir(&root)?.any(|entry| {
        entry
            .expect("payload directory entry should be readable")
            .path()
            .extension()
            .is_some_and(|extension| extension == "payload" || extension == "stage")
    }));
    Ok(())
}
