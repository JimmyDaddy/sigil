use anyhow::Result;

use super::{
    provider_continuation_payload_coordinator::ProviderContinuationPayloadCoordinatorInner,
    provider_continuation_payload_store::{
        InMemoryProviderContinuationSessionKeyStore, ProviderContinuationPayloadStore,
    },
    provider_native_compaction::NativeProviderCompactionAttemptInner,
    *,
};
use crate::{
    CompletionRequest, FrozenProviderRequestMaterial, ModelMessage, VersionedProfileIdentity,
};

fn profile(id: &str) -> VersionedProfileIdentity {
    VersionedProfileIdentity::from_content(id, 1, id.as_bytes())
}

fn request(session_scope_id: &str) -> Result<FrozenProviderRequestMaterial> {
    FrozenProviderRequestMaterial::freeze(
        session_scope_id,
        CompletionRequest {
            provider_name: "openai_responses".to_owned(),
            model_name: "gpt-4.1".to_owned(),
            messages: vec![ModelMessage::user("preserve this canonical window")],
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            reasoning_effort: None,
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: None,
            background: false,
            store: false,
            deterministic_materialization: true,
            hosted_tools: Vec::new(),
        },
    )
}

fn metadata(session_scope_id: &str) -> Result<NativeProviderCompactionMetadata> {
    Ok(NativeProviderCompactionMetadata {
        provider_route_fingerprint: provider_continuation_route_fingerprint(
            session_scope_id,
            "openai_responses",
            "https://api.openai.com/v1",
        )?,
        model_metadata_profile: profile("openai-responses-model-metadata"),
        wire_profile: profile("openai-responses-wire"),
        wire_protocol: "openai_responses".to_owned(),
        wire_schema_version: "compact-v1".to_owned(),
        composition_profile: profile("openai-responses-compaction-output"),
        artifact_kind: "responses_compaction_output".to_owned(),
        sensitivity: ContextSensitivity::Repository,
    })
}

#[tokio::test]
async fn native_attempt_materializes_one_encrypted_opaque_window_in_causal_order() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("openai_responses", "gpt-4.1").with_store(store.clone());
    let start = store.append_compaction_started(CompactionStartedEntry {
        attempt_id: "seed-compaction".to_owned(),
        fallback_parent: CompactionFallbackParent::Root,
        initiation: CompactionInitiation::Manual,
        base_projection_revision: "native-compaction-test-r1".to_owned(),
        started_at_unix_ms: 1,
    })?;
    let session_scope_id = session.session_scope_id().to_owned();
    let payload_store = ProviderContinuationPayloadStore::new(
        temp.path().join("payloads"),
        session_scope_id.clone(),
        InMemoryProviderContinuationSessionKeyStore::default(),
    )?;
    let coordinator = ProviderContinuationPayloadCoordinatorInner::with_payload_store(
        store.clone(),
        payload_store,
    );
    let native_request = NativeProviderCompactionRequest {
        logical_run_id: "native-compaction-run-1".to_owned(),
        frozen_request: request(&session_scope_id)?,
        covers_through: CompactionCursor {
            session_id: session_scope_id.clone(),
            through_stream_sequence: start.stream_sequence,
            through_event_id: start.event_id,
        },
        metadata: metadata(&session_scope_id)?,
    };

    let mut attempt = NativeProviderCompactionAttemptInner::start(
        &session,
        store.clone(),
        coordinator,
        native_request,
    )
    .await?;
    let opaque_payload = br#"[{"type":"compaction","encrypted_content":"opaque-window-secret","extension":{"retain":true}}]"#.to_vec();
    let materialized = attempt
        .materialize_artifact("resp_compact_1".to_owned(), opaque_payload)
        .await?;
    attempt
        .finish(
            ProviderPhysicalAttemptOutcome::Completed,
            Some("resp_compact_1".to_owned()),
        )
        .await?;

    let records = JsonlSessionStore::read_event_records(store.path())?;
    let attempts = ProviderPhysicalAttemptProjection::from_records(&records)?;
    let native_attempt = attempts
        .attempt(&materialized.physical_attempt_id)
        .expect("native physical attempt should be durable");
    assert_eq!(
        native_attempt.entry.purpose,
        ProviderPhysicalAttemptPurpose::NativeCompaction
    );
    assert!(matches!(
        native_attempt.terminal.as_ref(),
        Some(ProviderPhysicalAttemptTerminalEntry {
            outcome: ProviderPhysicalAttemptOutcome::Completed,
            durable_output_event_ids,
            ..
        }) if durable_output_event_ids.len() == 2
    ));
    let projection = ProviderContinuationProjection::from_records(&records)?;
    let candidate = projection
        .candidate(&materialized.candidate_id)
        .expect("candidate should be durable after encrypted manifest commit");
    assert_eq!(
        candidate.entry.candidate.payload().payload_id,
        materialized.payload_id
    );
    assert!(
        projection
            .payload(&materialized.payload_id)
            .expect("payload state should project")
            .candidate_event_id
            .is_some()
    );

    let raw_jsonl = std::fs::read_to_string(store.path())?;
    assert!(!raw_jsonl.contains("opaque-window-secret"));
    assert!(
        temp.path()
            .join("payloads")
            .read_dir()?
            .any(|entry| entry.is_ok())
    );
    Ok(())
}

#[tokio::test]
async fn native_attempt_rejects_a_cursor_outside_the_durable_session_before_it_starts() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("openai_responses", "gpt-4.1").with_store(store.clone());
    let session_scope_id = session.session_scope_id().to_owned();
    let payload_store = ProviderContinuationPayloadStore::new(
        temp.path().join("payloads"),
        session_scope_id.clone(),
        InMemoryProviderContinuationSessionKeyStore::default(),
    )?;
    let coordinator = ProviderContinuationPayloadCoordinatorInner::with_payload_store(
        store.clone(),
        payload_store,
    );
    let native_request = NativeProviderCompactionRequest {
        logical_run_id: "native-compaction-run-2".to_owned(),
        frozen_request: request(&session_scope_id)?,
        covers_through: CompactionCursor {
            session_id: "other-session".to_owned(),
            through_stream_sequence: 1,
            through_event_id: "event-other".to_owned(),
        },
        metadata: metadata(&session_scope_id)?,
    };

    let error = match NativeProviderCompactionAttemptInner::start(
        &session,
        store.clone(),
        coordinator,
        native_request,
    )
    .await
    {
        Ok(_) => {
            anyhow::bail!("foreign cursor must fail before the native provider attempt starts")
        }
        Err(error) => error,
    };
    assert!(error.to_string().contains("different session scope"));
    assert!(JsonlSessionStore::read_event_records(store.path())?.is_empty());
    Ok(())
}
