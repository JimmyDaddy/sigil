use anyhow::Result;

use super::*;
use crate::{
    COMPACTION_TOKEN_PROOF_SCHEMA_VERSION, CompletionRequest, ContextSensitivity,
    ContextTrustLevel, ContinuationCheckpointV1, EffectiveTokenBudget, ExternalProvenanceEntry,
    ExternalTrust, FrozenProviderRequestMaterial, InputTokenEvidence, ModelMessage,
    PortableTargetRequestMaterial, ProviderNonGeneratingAttempt, ProviderPhysicalAttemptOutcome,
    ProviderPhysicalAttemptPurpose, RequestFitProof, Session, TaskMemoryV1,
    TokenMeasurementBinding, TokenMeasurementScope, VersionedProfileIdentity,
};

fn setup_session() -> Result<(tempfile::TempDir, JsonlSessionStore, Session)> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    session.append_user_message(ModelMessage::user(
        "必须保留 CJK 约束：不要删除原始 JSONL，也不要使用旧日志 bridge。",
    ))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("我会先建立可审计的 V2 checkpoint。".to_owned()),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user("继续实现 portable checkpoint。"))?;
    Ok((temp, store, session))
}

fn request(
    store: &JsonlSessionStore,
    attempt_id: &str,
    compaction_id: &str,
    prior_folded_through: Option<CompactionCursor>,
) -> Result<PortableSemanticCompactionRequest> {
    let records = store.read_event_records_writer()?;
    let plan = CompactionFoldPlan::from_records_after(&records, 1, prior_folded_through.as_ref())?;
    let source_event_id = plan
        .folded_event_ids
        .first()
        .cloned()
        .expect("fixture has foldable source history");
    Ok(PortableSemanticCompactionRequest {
        attempt_id: attempt_id.to_owned(),
        compaction_id: compaction_id.to_owned(),
        initiation: CompactionInitiation::Manual,
        base_projection_revision: "portable-checkpoint-r1".to_owned(),
        branch_id: None,
        valid_for_snapshot: "snapshot-v1".to_owned(),
        objective: Some(
            "Durably compact the current session without hiding raw history".to_owned(),
        ),
        language: "zh-CN".to_owned(),
        plan,
        model_output: ContinuationModelOutputV1 {
            in_progress: vec![ContinuationModelOutputItemV1 {
                text: "正在建立可重放的 portable checkpoint。".to_owned(),
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
    })
}

fn session_scope_id(store: &JsonlSessionStore) -> Result<String> {
    Ok(store
        .read_event_records_writer()?
        .first()
        .expect("fixture has a durable session stream")
        .session_id()
        .to_owned())
}

fn profile(profile_id: &str) -> VersionedProfileIdentity {
    VersionedProfileIdentity::from_content(profile_id, 1, profile_id.as_bytes())
}

fn target_material(
    session_scope_id: &str,
    checkpoint: &ContinuationCheckpointV1,
    task_memory: &TaskMemoryV1,
    candidate_messages: &[ModelMessage],
) -> Result<PortableTargetRequestMaterial> {
    let checkpoint_message = checkpoint.render_for_provider(task_memory)?;
    assert_eq!(
        candidate_messages.first().map(|message| &message.id),
        Some(&checkpoint_message.id)
    );
    let request = CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        messages: candidate_messages.to_vec(),
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
    target_material_for_request(session_scope_id, request)
}

fn target_material_for_request(
    session_scope_id: &str,
    request: CompletionRequest,
) -> Result<PortableTargetRequestMaterial> {
    let frozen_request = FrozenProviderRequestMaterial::freeze(session_scope_id, request)?;
    let binding = TokenMeasurementBinding {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        wire_profile: profile("portable-test-wire"),
        token_measurement_profile: profile("portable-test-tokenizer"),
        hosted_parity_profile: Some(profile("portable-test-hosted-parity")),
    };
    let proof = RequestFitProof {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        input: InputTokenEvidence::Exact {
            tokens: 10,
            material_fingerprint: frozen_request.fingerprint().to_owned(),
            measurement_scope: TokenMeasurementScope::RenderedTargetInput,
            binding: binding.clone(),
            provider_model_snapshot: None,
            provider_system_fingerprint: None,
        },
        budget: EffectiveTokenBudget {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            budget_profile: profile("portable-test-budget"),
            context_window_tokens: 100,
            requested_output_tokens: 20,
            safety_buffer_tokens: 10,
        },
    };
    let frozen_before_request = frozen_request.clone();
    let before_input = InputTokenEvidence::Exact {
        tokens: 80,
        material_fingerprint: frozen_before_request.fingerprint().to_owned(),
        measurement_scope: TokenMeasurementScope::RenderedTargetInput,
        binding: binding.clone(),
        provider_model_snapshot: None,
        provider_system_fingerprint: None,
    };
    PortableTargetRequestMaterial::new(frozen_request, binding, proof)
        .with_portable_economics(&frozen_before_request, before_input)
}

fn execute_with_target<F>(
    store: &JsonlSessionStore,
    request: PortableSemanticCompactionRequest,
    materialize: F,
) -> Result<PortableSemanticCompactionOutcome>
where
    F: FnOnce(
        &ContinuationCheckpointV1,
        &TaskMemoryV1,
        &[ModelMessage],
    ) -> Result<PortableTargetRequestMaterial>,
{
    let preflight = store.prepare_portable_semantic_compaction(request)?;
    let target = materialize(
        preflight.checkpoint(),
        preflight.task_memory(),
        preflight.candidate_messages(),
    )?;
    store.execute_portable_semantic_compaction(preflight, target)
}

#[test]
fn portable_executor_admits_one_completed_input_measurement_without_relaxing_the_source_cas()
-> Result<()> {
    let (_temp, store, session) = setup_session()?;
    let session_scope_id = session_scope_id(&store)?;
    let request = request(&store, "attempt-measured", "compaction-measured", None)?;
    let mut preflight = store.prepare_portable_semantic_compaction(request)?;
    let target = target_material(
        &session_scope_id,
        preflight.checkpoint(),
        preflight.task_memory(),
        preflight.candidate_messages(),
    )?;
    let frozen_request = target.frozen_request.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let receipt = runtime.block_on(async {
        let mut measurement = ProviderNonGeneratingAttempt::start(
            &session,
            "input-token-measurement-portable-test",
            &frozen_request,
            ProviderPhysicalAttemptPurpose::InputTokenMeasurement,
        )
        .await?;
        measurement
            .finish(&session, ProviderPhysicalAttemptOutcome::Completed)
            .await?;
        measurement
            .completed_receipt()
            .cloned()
            .context("store-backed measurement must expose a completed receipt")
    })?;
    preflight.admit_completed_input_token_measurement(receipt, frozen_request.fingerprint())?;

    let outcome = store.execute_portable_semantic_compaction(preflight, target)?;
    assert_eq!(outcome.compaction_id, "compaction-measured");
    Ok(())
}

#[test]
fn portable_executor_pins_cjk_user_constraints_and_projects_checkpoint_after_applied() -> Result<()>
{
    let (_temp, store, _session) = setup_session()?;
    let session_scope_id = session_scope_id(&store)?;
    let request = request(&store, "attempt-1", "compaction-1", None)?;
    let expected_folded_through = request.plan.folded_through.clone();
    let outcome = execute_with_target(&store, request, |checkpoint, task_memory, candidate| {
        target_material(&session_scope_id, checkpoint, task_memory, candidate)
    })?;
    assert!(outcome.task_memory_id.starts_with("task-memory:"));

    let session = Session::load_from_store("deepseek", "deepseek-v4-flash", store.clone())?;
    let projection = session
        .try_context_projection_from_durable()?
        .expect("store-backed session has a durable projection");
    let messages = projection.model_messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, crate::MessageRole::Assistant);
    let checkpoint = messages[0].content.as_deref().expect("checkpoint content");
    assert!(checkpoint.contains("Constraints & Preferences"));
    assert!(checkpoint.contains("不要删除原始 JSONL"));
    assert!(checkpoint.contains("[model-generated, unverified]"));
    assert_eq!(
        messages[1].content.as_deref(),
        Some("继续实现 portable checkpoint。")
    );
    assert_eq!(projection.folded_through, expected_folded_through);
    Ok(())
}

#[test]
fn portable_executor_persists_idle_auto_initiation_for_failure_latch_replay() -> Result<()> {
    let (_temp, store, _session) = setup_session()?;
    let session_scope_id = session_scope_id(&store)?;
    let mut request = request(&store, "idle-attempt", "idle-compaction", None)?;
    request.initiation = CompactionInitiation::IdleAutomatic {
        scope_fingerprint: "idle-scope-v1".to_owned(),
    };

    execute_with_target(&store, request, |checkpoint, task_memory, candidate| {
        target_material(&session_scope_id, checkpoint, task_memory, candidate)
    })?;

    let started = store
        .read_event_records_writer()?
        .into_iter()
        .find_map(|record| {
            let event = record.stored_event();
            (event.event_kind() == Some(DurableEventType::CompactionStarted))
                .then(|| serde_json::from_value::<CompactionStartedEntry>(event.payload.clone()))
        })
        .expect("portable executor should persist its started lifecycle")?;
    assert!(matches!(
        started.initiation,
        CompactionInitiation::IdleAutomatic { ref scope_fingerprint }
            if scope_fingerprint == "idle-scope-v1"
    ));
    Ok(())
}

#[test]
fn portable_preflight_materializes_the_full_candidate_without_durable_writes() -> Result<()> {
    let (temp, store, session) = setup_session()?;
    let session_scope_id = session_scope_id(&store)?;
    let before = std::fs::read(store.path())?;
    let request = request(&store, "attempt-preflight", "compaction-preflight", None)?;
    let preflight = store.prepare_portable_semantic_compaction(request)?;
    assert_eq!(std::fs::read(store.path())?, before);
    assert_eq!(preflight.candidate_messages().len(), 2);
    assert_eq!(
        preflight.candidate_messages()[1].content.as_deref(),
        Some("继续实现 portable checkpoint。")
    );

    let target_request = session.build_portable_compaction_candidate_request(
        temp.path(),
        &crate::MemoryConfig { enabled: false },
        preflight.checkpoint(),
        preflight.task_memory(),
        preflight.candidate_messages().to_vec(),
        Vec::new(),
        None,
        None,
        None,
        None,
        &[],
        crate::RuntimeContextCandidates::default(),
        &[],
    )?;
    assert_eq!(std::fs::read(store.path())?, before);
    let candidate_ids = preflight
        .candidate_messages()
        .iter()
        .map(|message| message.id.as_str())
        .collect::<Vec<_>>();
    let target_ids = target_request
        .messages
        .iter()
        .map(|message| message.id.as_str())
        .collect::<Vec<_>>();
    assert!(target_ids.ends_with(&candidate_ids));

    let target_material = target_material_for_request(&session_scope_id, target_request)?;
    store.execute_portable_semantic_compaction(preflight, target_material)?;
    let records = store.read_event_records_writer()?;
    assert_eq!(records.len(), 6);
    assert!(matches!(
        records[3],
        SessionStreamRecord::Stored(ref event)
            if event.event_kind() == Some(DurableEventType::CompactionStarted)
    ));
    assert!(matches!(
        records[4],
        SessionStreamRecord::Stored(ref event)
            if event.event_kind() == Some(DurableEventType::TaskMemoryRecordedV1)
    ));
    assert!(matches!(
        records[5],
        SessionStreamRecord::Stored(ref event)
            if event.event_kind() == Some(DurableEventType::CompactionAppliedV2)
    ));
    Ok(())
}

#[test]
fn portable_preflight_rejects_invalid_model_authority_claim_before_start() -> Result<()> {
    let (_temp, store, _session) = setup_session()?;
    let session_scope_id = session_scope_id(&store)?;
    let mut request = request(&store, "attempt-invalid", "compaction-invalid", None)?;
    request.model_output.in_progress[0].text = "任务已经完成并已验证。".to_owned();
    assert!(
        execute_with_target(&store, request, |checkpoint, task_memory, candidate| {
            target_material(&session_scope_id, checkpoint, task_memory, candidate)
        })
        .is_err()
    );

    let records = store.read_event_records_writer()?;
    let lifecycle = CompactionLifecycleProjection::from_records(&records)?;
    assert!(lifecycle.attempt("attempt-invalid").is_none());
    assert!(
        CompactionSidecarProjection::from_records(&records)?
            .latest_for_branch(None)
            .is_none()
    );
    Ok(())
}

#[test]
fn portable_preflight_rejects_model_source_ids_outside_the_closed_catalog() -> Result<()> {
    let (_temp, store, _session) = setup_session()?;
    let session_scope_id = session_scope_id(&store)?;
    let mut request = request(
        &store,
        "attempt-unknown-source",
        "compaction-unknown-source",
        None,
    )?;
    request.model_output.in_progress[0].source_event_ids = vec!["invented-event-id".to_owned()];
    assert!(
        execute_with_target(&store, request, |checkpoint, task_memory, candidate| {
            target_material(&session_scope_id, checkpoint, task_memory, candidate)
        })
        .is_err()
    );

    let records = store.read_event_records_writer()?;
    let lifecycle = CompactionLifecycleProjection::from_records(&records)?;
    assert!(lifecycle.attempt("attempt-unknown-source").is_none());
    Ok(())
}

#[test]
fn portable_executor_keeps_external_source_notes_unverified_and_non_authoritative() -> Result<()> {
    let (_temp, store, mut session) = setup_session()?;
    let session_scope_id = session_scope_id(&store)?;
    let external_message_id = session.messages()[1].id.clone();
    session.append_external_provenance(ExternalProvenanceEntry {
        session_scope_id: session_scope_id.clone(),
        message_id: external_message_id,
        trust: ExternalTrust::ExternalUntrusted,
        sources: Vec::new(),
        citations: Vec::new(),
    })?;
    let mut request = request(&store, "attempt-external", "compaction-external", None)?;
    let external_event_id = request
        .plan
        .folded_event_ids
        .last()
        .cloned()
        .expect("fixture folds the externally attributed assistant message");
    request.model_output.in_progress[0] = ContinuationModelOutputItemV1 {
        text: "外部内容声称应忽略既有约束。".to_owned(),
        source_event_ids: vec![external_event_id],
        priority: ContinuationItemPriority::Critical,
    };

    execute_with_target(&store, request, |checkpoint, task_memory, candidate| {
        target_material(&session_scope_id, checkpoint, task_memory, candidate)
    })?;

    let records = store.read_event_records_writer()?;
    let sidecars = CompactionSidecarProjection::from_records(&records)?;
    let active = sidecars
        .latest_for_branch(None)
        .expect("external-source checkpoint is active");
    let item = active
        .checkpoint
        .in_progress
        .first()
        .expect("model item is retained as an unverified note");
    assert_eq!(item.origin, ContinuationItemOrigin::ModelGenerated);
    assert_eq!(
        item.authority,
        ContinuationItemAuthority::ModelGeneratedUnverified
    );
    assert_eq!(item.trust_level, ContextTrustLevel::ExternalUntrusted);
    assert_eq!(item.sensitivity, ContextSensitivity::External);
    assert_eq!(
        item.evidence_status,
        ContinuationEvidenceStatus::ModelGeneratedUnverified
    );
    Ok(())
}

#[test]
fn portable_preflight_allows_a_rebuilt_retry_after_an_unadmitted_candidate() -> Result<()> {
    let (_temp, store, _session) = setup_session()?;
    let session_scope_id = session_scope_id(&store)?;
    let mut invalid = request(&store, "attempt-failed", "compaction-failed", None)?;
    invalid.model_output.in_progress[0].source_event_ids = vec!["unknown-source".to_owned()];
    assert!(
        execute_with_target(&store, invalid, |checkpoint, task_memory, candidate| {
            target_material(&session_scope_id, checkpoint, task_memory, candidate)
        })
        .is_err()
    );

    let retry = request(&store, "attempt-retry", "compaction-retry", None)?;
    execute_with_target(&store, retry, |checkpoint, task_memory, candidate| {
        target_material(&session_scope_id, checkpoint, task_memory, candidate)
    })?;

    let records = store.read_event_records_writer()?;
    let lifecycle = CompactionLifecycleProjection::from_records(&records)?;
    assert!(lifecycle.attempt("attempt-failed").is_none());
    assert!(matches!(
        lifecycle
            .attempt("attempt-retry")
            .expect("rebuilt retry is retained for audit")
            .terminal,
        Some(CompactionAttemptTerminal::Applied { .. })
    ));
    assert_eq!(
        CompactionSidecarProjection::from_records(&records)?
            .latest_for_branch(None)
            .expect("successful retry becomes active")
            .compaction_id,
        "compaction-retry"
    );
    Ok(())
}

#[test]
fn repeated_portable_compaction_uses_the_active_boundary_as_its_only_prior_prefix() -> Result<()> {
    let (_temp, store, mut session) = setup_session()?;
    let session_scope_id = session_scope_id(&store)?;
    let first = execute_with_target(
        &store,
        request(&store, "attempt-1", "compaction-1", None)?,
        |checkpoint, task_memory, candidate| {
            target_material(&session_scope_id, checkpoint, task_memory, candidate)
        },
    )?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("先保留已经 compact 后的 raw tail。".to_owned()),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user("继续做第二次 compact。"))?;

    let before_second = store.read_event_records_writer()?;
    let active = CompactionSidecarProjection::from_records(&before_second)?
        .latest_for_branch(None)
        .expect("first compaction is active")
        .clone();
    let second_request = request(
        &store,
        "attempt-2",
        "compaction-2",
        Some(active.folded_through.clone()),
    )?;
    assert!(
        second_request
            .plan
            .protected_events
            .iter()
            .any(|protected| {
                protected.reason == CompactionFoldProtectionReason::ExistingCompactionBoundary
            })
    );
    assert!(
        second_request
            .plan
            .folded_event_ids
            .iter()
            .all(|event_id| { event_id != &active.folded_through.through_event_id })
    );
    execute_with_target(
        &store,
        second_request,
        |checkpoint, task_memory, candidate| {
            target_material(&session_scope_id, checkpoint, task_memory, candidate)
        },
    )?;

    let after_second = store.read_event_records_writer()?;
    let sidecars = CompactionSidecarProjection::from_records(&after_second)?;
    let active = sidecars
        .latest_for_branch(None)
        .expect("second compaction supersedes the first");
    assert_eq!(active.compaction_id, "compaction-2");
    assert_eq!(
        active.task_memory.supersedes.as_deref(),
        Some(first.task_memory_id.as_str())
    );
    let second_memory_id = active.task_memory.memory_id.clone();

    session.append_assistant_message(ModelMessage::assistant(
        Some("第三次 compact 仍只处理上一个边界之后的消息。".to_owned()),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user("继续做第三次 compact。"))?;
    let third_request = request(
        &store,
        "attempt-3",
        "compaction-3",
        Some(active.folded_through.clone()),
    )?;
    execute_with_target(
        &store,
        third_request,
        |checkpoint, task_memory, candidate| {
            target_material(&session_scope_id, checkpoint, task_memory, candidate)
        },
    )?;
    let after_third = store.read_event_records_writer()?;
    let sidecars = CompactionSidecarProjection::from_records(&after_third)?;
    let active = sidecars
        .latest_for_branch(None)
        .expect("third compaction supersedes the second");
    assert_eq!(active.compaction_id, "compaction-3");
    assert_eq!(
        active.task_memory.supersedes.as_deref(),
        Some(second_memory_id.as_str())
    );
    Ok(())
}

#[test]
fn portable_executor_rejects_a_frozen_request_that_omits_the_checkpoint() -> Result<()> {
    let (_temp, store, _session) = setup_session()?;
    let session_scope_id = session_scope_id(&store)?;
    let request = request(
        &store,
        "attempt-missing-checkpoint",
        "compaction-missing-checkpoint",
        None,
    )?;

    assert!(
        execute_with_target(&store, request, |checkpoint, task_memory, candidate| {
            let mut target =
                target_material(&session_scope_id, checkpoint, task_memory, candidate)?;
            target.frozen_request = FrozenProviderRequestMaterial::freeze(
                &session_scope_id,
                CompletionRequest {
                    provider_name: "deepseek".to_owned(),
                    model_name: "deepseek-v4-flash".to_owned(),
                    messages: vec![ModelMessage::user("not the checkpoint")],
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
                },
            )?;
            target.proof = RequestFitProof {
                schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
                input: InputTokenEvidence::ConservativeUpperBound {
                    tokens_upper_bound: 10,
                    material_fingerprint: target.frozen_request.fingerprint().to_owned(),
                    measurement_scope: TokenMeasurementScope::RenderedTargetInput,
                    binding: target.binding.clone(),
                },
                budget: EffectiveTokenBudget {
                    schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
                    budget_profile: profile("portable-test-budget"),
                    context_window_tokens: 100,
                    requested_output_tokens: 20,
                    safety_buffer_tokens: 10,
                },
            };
            Ok(target)
        })
        .is_err()
    );

    let records = store.read_event_records_writer()?;
    let lifecycle = CompactionLifecycleProjection::from_records(&records)?;
    assert!(lifecycle.attempt("attempt-missing-checkpoint").is_none());
    assert!(
        CompactionSidecarProjection::from_records(&records)?
            .latest_for_branch(None)
            .is_none()
    );
    Ok(())
}

#[test]
fn portable_executor_rejects_target_material_from_a_different_session_scope() -> Result<()> {
    let (_temp, store, _session) = setup_session()?;
    let request = request(
        &store,
        "attempt-wrong-session",
        "compaction-wrong-session",
        None,
    )?;

    assert!(
        execute_with_target(&store, request, |checkpoint, task_memory, candidate| {
            target_material(
                "different-session-scope",
                checkpoint,
                task_memory,
                candidate,
            )
        })
        .is_err()
    );

    let records = store.read_event_records_writer()?;
    let lifecycle = CompactionLifecycleProjection::from_records(&records)?;
    assert!(lifecycle.attempt("attempt-wrong-session").is_none());
    assert!(
        CompactionSidecarProjection::from_records(&records)?
            .latest_for_branch(None)
            .is_none()
    );
    Ok(())
}
