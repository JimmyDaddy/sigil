use std::fs;

use anyhow::Result;
use serde_json::json;

use super::*;
use crate::{
    AssistantMessageKind, COMPACTION_TOKEN_PROOF_SCHEMA_VERSION, CompactionFoldPlan,
    CompactionInitiation, CompletionRequest, ContinuationCheckpointV1, ContinuationItemPriority,
    ContinuationModelOutputItemV1, ContinuationModelOutputV1, ControlledCheckpointProjection,
    ConversationInputKind, ConversationInputPromotedEntry, ConversationInputQueueId,
    ConversationInputQueuedEntry, ConversationInputTarget, EffectiveTokenBudget,
    ExternalEvidenceLevel, ExternalSourceRecord, ExternalTrust, FrozenProviderRequestMaterial,
    InputTokenEvidence, ModelMessage, MutationEventRecorder, PortableSemanticCompactionRequest,
    PortableTargetRequestMaterial, RequestFitProof, SourceCacheStatus, SourceFreshness,
    TaskMemoryV1, TokenMeasurementBinding, TokenMeasurementScope, ToolOutputProjectionPolicy,
    ToolRestartPolicy, UsageStats, VersionedProfileIdentity,
    conversation_promotion_capability_digest, project_conversation_prompt_for_persistence,
    write_file_with_mutation,
};

fn portable_target_profile(profile_id: &str) -> VersionedProfileIdentity {
    VersionedProfileIdentity::from_content(profile_id, 1, profile_id.as_bytes())
}

fn portable_target_material(
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
    let frozen_request = FrozenProviderRequestMaterial::freeze(
        session_scope_id,
        CompletionRequest {
            provider_name: "deepseek".to_owned(),
            model_name: "chat".to_owned(),
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
        },
    )?;
    let binding = TokenMeasurementBinding {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        provider_name: "deepseek".to_owned(),
        model_name: "chat".to_owned(),
        wire_profile: portable_target_profile("fork-portable-wire"),
        token_measurement_profile: portable_target_profile("fork-portable-tokenizer"),
        hosted_parity_profile: Some(portable_target_profile("fork-portable-hosted-parity")),
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
            budget_profile: portable_target_profile("fork-portable-budget"),
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

#[test]
fn conversation_fork_copies_safe_prefix_rebinds_provenance_and_preserves_parent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifacts = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let note = workspace.join("note.txt");
    fs::write(&note, "before\n")?;
    let source_path = temp.path().join("source.jsonl");
    let source_store = JsonlSessionStore::new(&source_path)?;
    let mut source = Session::new("deepseek", "chat").with_store(source_store.clone());
    source.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "chat".to_owned(),
    })?;
    source.append_user_message(ModelMessage::user("edit note"))?;
    let recorder = MutationEventRecorder::with_artifact_root(source_store.clone(), artifacts);
    write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "call-edit",
        "note.txt",
        &note,
        b"after\n",
    )?;
    source.append_control(ControlEntry::UsageSnapshot(UsageStats::default()))?;
    let assistant = ModelMessage::assistant_with_kind(
        Some("Done with source".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    );
    source.append_assistant_message(assistant.clone())?;
    let external_source = ExternalSourceRecord::from_remote_candidate(
        source.session_scope_id(),
        Some("remote-1"),
        ExternalEvidenceLevel::SearchSnippet,
        "https://example.com/docs",
        "test",
        Some("Example".to_owned()),
        None,
        "2026-07-13T00:00:00Z",
        None,
        Some(1),
        SourceFreshness::Fresh,
        SourceCacheStatus::Miss,
        ToolRestartPolicy::Replayable,
    )?;
    source.append_external_provenance(ExternalProvenanceEntry {
        session_scope_id: source.session_scope_id().to_owned(),
        message_id: assistant.id.clone(),
        trust: ExternalTrust::ExternalUntrusted,
        sources: vec![external_source],
        citations: Vec::new(),
    })?;
    source.append_durable_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({
            "run_status": "completed",
            "terminal_reason": "final_answer",
            "final_message_id": assistant.id,
            "tool_calls": 1,
            "error": null
        }),
    )?;
    let compaction_source_records = JsonlSessionStore::read_event_records(&source_path)?;
    let plan = CompactionFoldPlan::from_records(&compaction_source_records, 1)?;
    let model_source_event_id = plan
        .folded_event_ids
        .first()
        .cloned()
        .expect("fork fixture has foldable user history");
    let source_session_scope_id = source.session_scope_id().to_owned();
    let portable_request = PortableSemanticCompactionRequest {
        attempt_id: "fork-portable-attempt".to_owned(),
        compaction_id: "fork-portable-compaction".to_owned(),
        initiation: CompactionInitiation::Manual,
        base_projection_revision: "fork-checkpoint-r1".to_owned(),
        branch_id: None,
        valid_for_snapshot: "fork-snapshot-v1".to_owned(),
        objective: Some("Preserve forked raw conversation history".to_owned()),
        language: "en".to_owned(),
        plan,
        model_output: ContinuationModelOutputV1 {
            in_progress: vec![ContinuationModelOutputItemV1 {
                text: "Preparing a safe conversation fork.".to_owned(),
                source_event_ids: vec![model_source_event_id],
                priority: ContinuationItemPriority::Normal,
            }],
            pending_actions: Vec::new(),
            provider_continuity: Vec::new(),
            model_notes: Vec::new(),
        },
        tool_output_projection_policy: ToolOutputProjectionPolicy::default(),
        started_at_unix_ms: 10,
        completed_at_unix_ms: 11,
    };
    let portable_preflight = source_store.prepare_portable_semantic_compaction(portable_request)?;
    let portable_target = portable_target_material(
        &source_session_scope_id,
        portable_preflight.checkpoint(),
        portable_preflight.task_memory(),
        portable_preflight.candidate_messages(),
    )?;
    source_store.execute_portable_semantic_compaction(portable_preflight, portable_target)?;
    let records = JsonlSessionStore::read_event_records(&source_path)?;
    let checkpoint = ControlledCheckpointProjection::from_records(&records)?
        .latest()
        .cloned()
        .expect("checkpoint");
    let before_parent = fs::read(&source_path)?;
    let destination_path = temp.path().join("fork.jsonl");

    let output = fork_conversation_at_checkpoint(
        &source_store,
        &records,
        &ConversationForkRequest {
            checkpoint_id: checkpoint.checkpoint_id,
            checkpoint_digest: checkpoint.checkpoint_digest,
            source_session_ref: SessionRef::new_relative("source.jsonl")?,
            destination_path: destination_path.clone(),
            provider_name: "deepseek".to_owned(),
            model_name: "chat".to_owned(),
        },
    )?;

    assert_eq!(fs::read(&source_path)?, before_parent);
    assert_eq!(
        output.destination_session_ref.as_path(),
        std::path::Path::new("fork.jsonl")
    );
    assert_eq!(output.copied_message_count, 2);
    assert_eq!(output.copied_external_provenance_count, 1);
    let destination_records = JsonlSessionStore::read_event_records(&destination_path)?;
    assert!(destination_records.iter().any(|record| matches!(
        record,
        SessionStreamRecord::Stored(event)
            if event.event_kind() == Some(DurableEventType::ConversationForked)
    )));
    assert!(!destination_records.iter().any(|record| matches!(
        record,
        SessionStreamRecord::Stored(event)
            if event.event_kind() == Some(DurableEventType::MutationCommitted)
    )));
    assert!(!destination_records.iter().any(|record| matches!(
        record,
        SessionStreamRecord::Stored(event)
            if matches!(
                event.event_kind(),
                Some(DurableEventType::CompactionAppliedV2 | DurableEventType::TaskMemoryRecordedV1)
            )
    )));
    let destination = Session::load_from_store(
        "fallback",
        "fallback",
        JsonlSessionStore::new(destination_path)?,
    )?;
    assert_eq!(destination.entries().len(), 4);
    assert_eq!(destination.messages().len(), 2);
    let rebound = destination.external_provenance_entries();
    assert_eq!(rebound.len(), 1);
    assert_eq!(rebound[0].session_scope_id, output.destination_session_id);
    assert!(
        rebound[0]
            .sources
            .iter()
            .all(|source| source.session_scope_id == output.destination_session_id)
    );
    Ok(())
}

#[test]
fn conversation_fork_rejects_unfinalized_turn_without_creating_destination() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let note = workspace.join("note.txt");
    fs::write(&note, "before\n")?;
    let source_path = temp.path().join("source.jsonl");
    let source_store = JsonlSessionStore::new(&source_path)?;
    source_store.append(&SessionLogEntry::User(ModelMessage::user("edit")))?;
    let recorder = MutationEventRecorder::with_artifact_root(
        source_store.clone(),
        temp.path().join("artifacts"),
    );
    write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "call-edit",
        "note.txt",
        &note,
        b"after\n",
    )?;
    let records = JsonlSessionStore::read_event_records(&source_path)?;
    let checkpoint = ControlledCheckpointProjection::from_records(&records)?
        .latest()
        .cloned()
        .expect("checkpoint");
    let destination_path = temp.path().join("fork.jsonl");

    let error = fork_conversation_at_checkpoint(
        &source_store,
        &records,
        &ConversationForkRequest {
            checkpoint_id: checkpoint.checkpoint_id,
            checkpoint_digest: checkpoint.checkpoint_digest,
            source_session_ref: SessionRef::new_relative("source.jsonl")?,
            destination_path: destination_path.clone(),
            provider_name: "deepseek".to_owned(),
            model_name: "chat".to_owned(),
        },
    )
    .expect_err("unfinished turn must not fork");

    assert!(error.to_string().contains("requires a finalized user turn"));
    assert!(!destination_path.exists());
    Ok(())
}

#[test]
fn conversation_turn_fork_supports_finalized_turn_without_file_mutations() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source_path = temp.path().join("source.jsonl");
    let source_store = JsonlSessionStore::new(&source_path)?;
    let mut source = Session::new("deepseek", "chat").with_store(source_store.clone());
    source.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "chat".to_owned(),
    })?;
    source.append_user_message(ModelMessage::user("explain the repository"))?;
    let assistant = ModelMessage::assistant_with_kind(
        Some("Here is the explanation.".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    );
    source.append_assistant_message(assistant.clone())?;
    source.append_durable_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({
            "run_status": "completed",
            "terminal_reason": "final_answer",
            "final_message_id": assistant.id,
            "tool_calls": 0,
            "error": null
        }),
    )?;
    let records = JsonlSessionStore::read_event_records(&source_path)?;
    assert!(
        ControlledCheckpointProjection::from_records(&records)?
            .latest()
            .is_none()
    );
    let point = ConversationForkProjection::from_records(&records)?
        .latest()
        .cloned()
        .expect("finalized turn");
    let before_parent = fs::read(&source_path)?;
    let destination_path = temp.path().join("fork.jsonl");

    let output = fork_conversation_at_turn(
        &source_store,
        &records,
        &ConversationTurnForkRequest {
            source_turn_digest: point.source_turn_digest.clone(),
            source_session_ref: SessionRef::new_relative("source.jsonl")?,
            destination_path: destination_path.clone(),
            provider_name: "deepseek".to_owned(),
            model_name: "chat".to_owned(),
        },
    )?;

    assert_eq!(fs::read(&source_path)?, before_parent);
    assert_eq!(output.copied_message_count, 2);
    let destination_records = JsonlSessionStore::read_event_records(destination_path)?;
    let fork_payload = destination_records
        .iter()
        .find_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_kind() == Some(DurableEventType::ConversationForked) =>
            {
                serde_json::from_value::<ConversationForked>(event.payload.clone()).ok()
            }
            _ => None,
        })
        .expect("fork provenance");
    assert!(fork_payload.source_checkpoint_id.is_none());
    assert!(fork_payload.source_checkpoint_digest.is_none());
    assert_eq!(fork_payload.source_turn_digest, point.source_turn_digest);
    Ok(())
}

#[test]
fn promoted_user_is_checkpoint_and_fork_boundary_without_a_second_user_event() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifacts = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let note = workspace.join("note.txt");
    fs::write(&note, "before\n")?;
    let source_path = temp.path().join("source.jsonl");
    let source_store = JsonlSessionStore::new(&source_path)?;
    let mut source = Session::new("deepseek", "chat").with_store(source_store.clone());
    source.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "chat".to_owned(),
    })?;
    let queue_id = ConversationInputQueueId::new("fork-promoted-user")?;
    let prompt = project_conversation_prompt_for_persistence("edit the promoted note");
    source.append_control(ControlEntry::ConversationInputQueued(
        ConversationInputQueuedEntry {
            queue_id: queue_id.clone(),
            target: ConversationInputTarget::MainThread,
            kind: ConversationInputKind::Chat,
            prompt_hash: prompt.prompt_hash.clone(),
            prompt: prompt.safe_prompt.clone(),
            reasoning_effort: None,
            created_at_ms: Some(1),
        },
    ))?;
    let revision = source
        .try_conversation_queue_durable_projection_from_durable()?
        .expect("durable queue projection")
        .revision
        .expect("queued event advances revision");
    let mut durable_user_message = ModelMessage::user(prompt.safe_prompt.clone());
    durable_user_message.id = "fork-promoted-message".to_owned();
    let promotion = ConversationInputPromotedEntry {
        queue_id,
        expected_queue_revision: revision,
        prompt_hash: prompt.prompt_hash,
        exact_prompt_required: false,
        durable_user_message: durable_user_message.clone(),
        capability_descriptors: Vec::new(),
        capability_digest: conversation_promotion_capability_digest(&[])?,
        dispatch_run_id: "fork-promoted-run".to_owned(),
        promoted_at_ms: 2,
    };
    let promotion_event = source_store.append_conversation_input_promoted(promotion)?;
    let recorder = MutationEventRecorder::with_artifact_root(source_store.clone(), artifacts);
    write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "call-promoted-edit",
        "note.txt",
        &note,
        b"after\n",
    )?;
    let assistant = ModelMessage::assistant_with_kind(
        Some("Done".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    );
    source.append_assistant_message(assistant.clone())?;
    source.append_durable_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({
            "run_status": "completed",
            "terminal_reason": "final_answer",
            "final_message_id": assistant.id,
            "tool_calls": 1,
            "error": null
        }),
    )?;

    let records = JsonlSessionStore::read_event_records(&source_path)?;
    assert_eq!(
        records
            .iter()
            .filter(|record| {
                record.stored_event().event_kind() == Some(DurableEventType::UserMessageRecorded)
            })
            .count(),
        0
    );
    let checkpoint = ControlledCheckpointProjection::from_records(&records)?
        .latest()
        .cloned()
        .expect("promoted mutation checkpoint");
    assert_eq!(checkpoint.prompt.as_deref(), Some("edit the promoted note"));
    assert_eq!(checkpoint.turn_boundary_event_id, promotion_event.event_id);
    let point = ConversationForkProjection::from_records(&records)?
        .latest()
        .cloned()
        .expect("promoted finalized turn");
    assert_eq!(point.source_boundary_event_id, promotion_event.event_id);
    let destination_path = temp.path().join("fork.jsonl");
    let output = fork_conversation_at_checkpoint(
        &source_store,
        &records,
        &ConversationForkRequest {
            checkpoint_id: checkpoint.checkpoint_id,
            checkpoint_digest: checkpoint.checkpoint_digest,
            source_session_ref: SessionRef::new_relative("source.jsonl")?,
            destination_path: destination_path.clone(),
            provider_name: "deepseek".to_owned(),
            model_name: "chat".to_owned(),
        },
    )?;
    assert_eq!(output.copied_message_count, 2);
    assert_eq!(
        JsonlSessionStore::read_entries(destination_path)?
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    SessionLogEntry::User(message)
                        if message.id == durable_user_message.id
                            && message.content == durable_user_message.content
                )
            })
            .count(),
        1
    );
    Ok(())
}

#[test]
fn conversation_turn_fork_rejects_stale_digest_before_creating_destination() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source_path = temp.path().join("source.jsonl");
    let source_store = JsonlSessionStore::new(&source_path)?;
    source_store.append(&SessionLogEntry::User(ModelMessage::user("hello")))?;
    source_store.append_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({"run_status": "completed"}),
    )?;
    let records = JsonlSessionStore::read_event_records(&source_path)?;
    let destination_path = temp.path().join("fork.jsonl");

    let error = fork_conversation_at_turn(
        &source_store,
        &records,
        &ConversationTurnForkRequest {
            source_turn_digest: "stale".to_owned(),
            source_session_ref: SessionRef::new_relative("source.jsonl")?,
            destination_path: destination_path.clone(),
            provider_name: "deepseek".to_owned(),
            model_name: "chat".to_owned(),
        },
    )
    .expect_err("stale digest must fail");

    assert!(
        error
            .to_string()
            .contains("changed or is no longer available")
    );
    assert!(!destination_path.exists());
    Ok(())
}
