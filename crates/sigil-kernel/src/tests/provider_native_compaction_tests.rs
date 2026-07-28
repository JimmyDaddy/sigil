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
    COMPACTION_TOKEN_PROOF_SCHEMA_VERSION, CompletionRequest, EffectiveTokenBudget,
    FrozenProviderRequestMaterial, InputTokenEvidence, ModelMessage, RequestFitProof,
    TokenMeasurementBinding, TokenMeasurementScope, VersionedProfileIdentity,
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

fn carrier_policy() -> NativeCarrierPolicyV1 {
    NativeCarrierPolicyV1 {
        provider_retention: ProviderRetentionPolicyV1::Disallowed,
        request_store_mode: false,
        expires_at_unix_ms: None,
    }
}

fn seed_portable_checkpoint(
    session: &mut Session,
    store: &JsonlSessionStore,
) -> Result<(CompactionId, CompactionCursor)> {
    session.append_user_message(ModelMessage::user("preserve portable root objective"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("portable progress".to_owned()),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user("continue with native acceleration"))?;
    let records = store.read_event_records_writer()?;
    let plan = CompactionFoldPlan::from_records_after(&records, 1, None)?;
    let source_event_id = plan
        .folded_event_ids
        .first()
        .cloned()
        .expect("portable fixture has foldable history");
    let compaction_id = "portable-before-native".to_owned();
    let preflight =
        store.prepare_portable_semantic_compaction(PortableSemanticCompactionRequest {
            attempt_id: "portable-before-native-attempt".to_owned(),
            compaction_id: compaction_id.clone(),
            initiation: CompactionInitiation::Manual,
            base_projection_revision: "native-dual-write-r1".to_owned(),
            branch_id: None,
            valid_for_snapshot: "snapshot-native-dual-write".to_owned(),
            objective: Some("portable truth remains authoritative".to_owned()),
            language: "en".to_owned(),
            plan,
            model_output: ContinuationModelOutputV1 {
                in_progress: vec![ContinuationModelOutputItemV1 {
                    text: "native carrier is optional".to_owned(),
                    source_event_ids: vec![source_event_id],
                    priority: ContinuationItemPriority::Critical,
                }],
                pending_actions: Vec::new(),
                provider_continuity: Vec::new(),
                model_notes: Vec::new(),
            },
            tool_output_projection_policy: ToolOutputProjectionPolicy::default(),
            started_at_unix_ms: 10,
            completed_at_unix_ms: 11,
        })?;
    let target_request = CompletionRequest {
        provider_name: "openai_responses".to_owned(),
        model_name: "gpt-4.1".to_owned(),
        messages: preflight.candidate_messages().to_vec(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(20),
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    };
    let frozen = FrozenProviderRequestMaterial::freeze(session.session_scope_id(), target_request)?;
    let binding = TokenMeasurementBinding {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        provider_name: "openai_responses".to_owned(),
        model_name: "gpt-4.1".to_owned(),
        wire_profile: profile("portable-native-wire"),
        token_measurement_profile: profile("portable-native-tokenizer"),
        hosted_parity_profile: Some(profile("portable-native-hosted-parity")),
    };
    let proof = RequestFitProof {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        input: InputTokenEvidence::Exact {
            tokens: 10,
            material_fingerprint: frozen.fingerprint().to_owned(),
            measurement_scope: TokenMeasurementScope::RenderedTargetInput,
            binding: binding.clone(),
            provider_model_snapshot: None,
            provider_system_fingerprint: None,
        },
        budget: EffectiveTokenBudget {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            budget_profile: profile("portable-native-budget"),
            context_window_tokens: 100,
            requested_output_tokens: 20,
            safety_buffer_tokens: 10,
        },
    };
    let before_input = InputTokenEvidence::Exact {
        tokens: 80,
        material_fingerprint: frozen.fingerprint().to_owned(),
        measurement_scope: TokenMeasurementScope::RenderedTargetInput,
        binding: binding.clone(),
        provider_model_snapshot: None,
        provider_system_fingerprint: None,
    };
    let target = PortableTargetRequestMaterial::new(frozen.clone(), binding, proof)
        .with_portable_economics(&frozen, before_input)?;
    store.execute_portable_semantic_compaction(preflight, target)?;
    let records = store.read_event_records_writer()?;
    let sidecar = CompactionSidecarProjection::from_records(&records)?
        .resolved_compaction(&compaction_id)
        .cloned()
        .expect("portable checkpoint is active");
    Ok((compaction_id, sidecar.folded_through))
}

#[tokio::test]
async fn native_attempt_materializes_one_encrypted_opaque_window_in_causal_order() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("openai_responses", "gpt-4.1").with_store(store.clone());
    let (portable_compaction_id, covers_through) = seed_portable_checkpoint(&mut session, &store)?;
    let session_scope_id = session.session_scope_id().to_owned();
    let key_store = InMemoryProviderContinuationSessionKeyStore::default();
    let payload_root = temp.path().join("payloads");
    let payload_store = ProviderContinuationPayloadStore::new(
        payload_root.clone(),
        session_scope_id.clone(),
        key_store.clone(),
    )?;
    let coordinator = ProviderContinuationPayloadCoordinatorInner::with_payload_store(
        store.clone(),
        payload_store,
    );
    let native_request = NativeProviderCompactionRequest {
        logical_run_id: "native-compaction-run-1".to_owned(),
        frozen_request: request(&session_scope_id)?,
        covers_through,
        portable_compaction_id: portable_compaction_id.clone(),
        carrier_policy: carrier_policy(),
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
    let ProviderContinuationCandidate::Artifact(reference) = &candidate.entry.candidate else {
        panic!("native compaction materializes an artifact");
    };
    let carrier = reference.carrier.as_ref().expect("V3 native carrier");
    assert_eq!(carrier.portable_compaction_id, portable_compaction_id);
    assert_eq!(
        carrier.portable_checkpoint_id,
        materialized.portable_checkpoint_id
    );
    assert_eq!(
        candidate.entry.resolution_mode,
        ProviderContinuationResolutionMode::NativePlusPortableModelCheckpoint
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
    assert!(payload_root.read_dir()?.any(|entry| entry.is_ok()));

    let resume_context = NativeCarrierResumeContextV1 {
        connection_fingerprint: carrier.connection_fingerprint.clone(),
        model_snapshot_id: carrier.model_snapshot_id.clone(),
        protocol_version: carrier.protocol_version.clone(),
        source_cursor: carrier.source_cursor.clone(),
        portable_compaction_id: carrier.portable_compaction_id.clone(),
        portable_checkpoint_id: carrier.portable_checkpoint_id.clone(),
        policy: carrier.policy.clone(),
        native_payload_available: true,
        portable_checkpoint_available: true,
        now_unix_ms: 20,
    };
    assert_eq!(
        carrier.resume_decision(&resume_context)?,
        NativeCarrierResumeDecisionV1::UseNative
    );
    let mut switched_model = resume_context.clone();
    switched_model.model_snapshot_id =
        "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_owned();
    assert_eq!(
        carrier.resume_decision(&switched_model)?,
        NativeCarrierResumeDecisionV1::UsePortable {
            reason: NativeCarrierInvalidationV1::ModelChanged,
        }
    );
    let mut forked_cursor = resume_context.clone();
    forked_cursor.source_cursor.session_id = "forked-session".to_owned();
    assert_eq!(
        carrier.resume_decision(&forked_cursor)?,
        NativeCarrierResumeDecisionV1::UsePortable {
            reason: NativeCarrierInvalidationV1::SourceCursorChanged,
        },
        "a fork must not inherit its parent's route-bound native carrier"
    );
    let mut retention_changed = resume_context.clone();
    retention_changed.policy.provider_retention = ProviderRetentionPolicyV1::Allowed;
    assert_eq!(
        carrier.resume_decision(&retention_changed)?,
        NativeCarrierResumeDecisionV1::UsePortable {
            reason: NativeCarrierInvalidationV1::RetentionPolicyChanged,
        }
    );
    let mut store_changed_carrier = carrier.clone();
    store_changed_carrier.policy.provider_retention = ProviderRetentionPolicyV1::Allowed;
    let mut store_changed = resume_context.clone();
    store_changed.policy.provider_retention = ProviderRetentionPolicyV1::Allowed;
    store_changed.policy.request_store_mode = true;
    assert_eq!(
        store_changed_carrier.resume_decision(&store_changed)?,
        NativeCarrierResumeDecisionV1::UsePortable {
            reason: NativeCarrierInvalidationV1::StoreModeChanged,
        }
    );
    let mut expiring_carrier = carrier.clone();
    expiring_carrier.policy.expires_at_unix_ms = Some(15);
    expiring_carrier
        .expires_or_invalidates_on
        .push(NativeCarrierInvalidationV1::Expired);
    assert_eq!(
        expiring_carrier.resume_decision(&resume_context)?,
        NativeCarrierResumeDecisionV1::UsePortable {
            reason: NativeCarrierInvalidationV1::Expired,
        }
    );
    let mut missing_payload = resume_context.clone();
    missing_payload.native_payload_available = false;
    assert_eq!(
        carrier.resume_decision(&missing_payload)?,
        NativeCarrierResumeDecisionV1::UsePortable {
            reason: NativeCarrierInvalidationV1::NativePayloadUnavailable,
        }
    );

    drop(attempt);
    for entry in payload_root.read_dir()? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "payload")
        {
            std::fs::remove_file(path)?;
        }
    }
    let recovery_payload_store =
        ProviderContinuationPayloadStore::new(payload_root, session_scope_id, key_store)?;
    let recovery = ProviderContinuationPayloadCoordinatorInner::with_payload_store(
        store.clone(),
        recovery_payload_store,
    )
    .recover()?;
    assert_eq!(recovery.portable_fallbacks, 1);

    let resumed = Session::new("openai_responses", "gpt-4.1").with_store(store.clone());
    let resumed_projection = resumed
        .try_context_projection_from_durable()?
        .expect("durable portable projection");
    assert_eq!(
        resumed_projection.active_compaction_id.as_deref(),
        Some(portable_compaction_id.as_str())
    );
    assert_eq!(
        resumed_projection
            .checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.continuity_v2.as_ref())
            .map(|continuity| continuity.checkpoint_id.as_str()),
        Some(materialized.portable_checkpoint_id.as_str())
    );
    let recovered_records = JsonlSessionStore::read_event_records(store.path())?;
    assert_eq!(
        recovered_records
            .iter()
            .filter(|record| {
                record.stored_event().event_type
                    == DurableEventType::ProviderPhysicalAttemptStarted.as_str()
            })
            .count(),
        1,
        "portable recovery must not start another provider/model attempt"
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
        portable_compaction_id: "missing-portable".to_owned(),
        carrier_policy: carrier_policy(),
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
