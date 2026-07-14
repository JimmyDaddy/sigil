use std::fs;

use anyhow::Result;
use serde_json::json;

use super::*;
use crate::{
    AssistantMessageKind, COMPACTION_TOKEN_PROOF_SCHEMA_VERSION, CompactionFoldPlan,
    CompactionInitiation, CompletionRequest, ContinuationCheckpointV1, ContinuationItemPriority,
    ContinuationModelOutputItemV1, ContinuationModelOutputV1, ControlledCheckpointProjection,
    EffectiveTokenBudget, ExternalEvidenceLevel, ExternalSourceRecord, ExternalTrust,
    FrozenProviderRequestMaterial, InputTokenEvidence, ModelMessage, MutationEventRecorder,
    PortableSemanticCompactionRequest, PortableTargetRequestMaterial, RequestFitProof,
    SourceCacheStatus, SourceFreshness, TaskMemoryV1, TokenMeasurementBinding,
    TokenMeasurementScope, ToolOutputProjectionPolicy, ToolRestartPolicy, UsageStats,
    VersionedProfileIdentity, write_file_with_mutation,
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
        hosted_parity_profile: None,
    };
    Ok(PortableTargetRequestMaterial {
        proof: RequestFitProof {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            input: InputTokenEvidence::ConservativeUpperBound {
                tokens_upper_bound: 10,
                material_fingerprint: frozen_request.fingerprint().to_owned(),
                measurement_scope: TokenMeasurementScope::RenderedTargetInput,
                binding: binding.clone(),
            },
            budget: EffectiveTokenBudget {
                schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
                budget_profile: portable_target_profile("fork-portable-budget"),
                context_window_tokens: 100,
                requested_output_tokens: 20,
                safety_buffer_tokens: 10,
            },
        },
        frozen_request,
        binding,
    })
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
