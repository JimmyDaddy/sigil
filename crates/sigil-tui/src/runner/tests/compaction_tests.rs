use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::{Stream, stream};
use sigil_kernel::{
    Agent, COMPACTION_TOKEN_PROOF_SCHEMA_VERSION, CompletionRequest, ControlEntry,
    DurableEventType, EffectiveTokenBudget, FrozenProviderRequestMaterial, InputTokenEvidence,
    JsonlSessionStore, ModelMessage, PortableTargetRequestMaterial, Provider, ProviderCapabilities,
    ProviderChunk, ProviderRequestRejection, ReasoningEffort, RequestFitProof, SessionLogEntry,
    TokenMeasurementBinding, TokenMeasurementScope, ToolRegistry, UsageStats,
    VersionedProfileIdentity,
};
use std::{
    collections::VecDeque,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tempfile::tempdir;

use super::{
    super::{V2CompactionAdmission, V2CompactionPreviewState, WorkerCommand, WorkerMessage},
    common::{PlannedProvider, StreamPlan, spawn_test_worker, test_root_config},
};

fn has_v2_compaction_lifecycle_event(path: &Path) -> Result<bool> {
    Ok(JsonlSessionStore::read_event_records(path)?
        .iter()
        .any(|record| {
            [
                DurableEventType::CompactionStarted,
                DurableEventType::CompactionAppliedV2,
                DurableEventType::CompactionFailed,
                DurableEventType::CompactionSkipped,
            ]
            .iter()
            .any(|expected| record.stored_event().event_type == expected.as_str())
        }))
}

#[derive(Clone)]
struct OverflowRecoveryProvider {
    plans: Arc<Mutex<VecDeque<StreamPlan>>>,
    stream_calls: Arc<Mutex<usize>>,
    target_proof_calls: Arc<Mutex<usize>>,
}

impl OverflowRecoveryProvider {
    fn new(plans: Vec<StreamPlan>) -> Self {
        Self {
            plans: Arc::new(Mutex::new(VecDeque::from(plans))),
            stream_calls: Arc::new(Mutex::new(0)),
            target_proof_calls: Arc::new(Mutex::new(0)),
        }
    }

    fn stream_calls(&self) -> usize {
        *self
            .stream_calls
            .lock()
            .expect("stream call mutex should not be poisoned")
    }

    fn target_proof_calls(&self) -> usize {
        *self
            .target_proof_calls
            .lock()
            .expect("target proof call mutex should not be poisoned")
    }
}

fn overflow_recovery_profile(profile_id: &str) -> VersionedProfileIdentity {
    VersionedProfileIdentity::from_content(profile_id, 1, profile_id.as_bytes())
}

fn overflow_recovery_target_material(
    frozen_request: FrozenProviderRequestMaterial,
) -> Result<PortableTargetRequestMaterial> {
    let request = frozen_request.request();
    let binding = TokenMeasurementBinding {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        provider_name: request.provider_name.clone(),
        model_name: request.model_name.clone(),
        wire_profile: overflow_recovery_profile("test-overflow-server-count-wire"),
        token_measurement_profile: overflow_recovery_profile("test-overflow-server-count"),
        hosted_parity_profile: Some(overflow_recovery_profile("test-overflow-server-parity")),
    };
    let proof = RequestFitProof {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        input: InputTokenEvidence::Exact {
            tokens: 1,
            material_fingerprint: frozen_request.fingerprint().to_owned(),
            measurement_scope: TokenMeasurementScope::RenderedTargetInput,
            binding: binding.clone(),
            provider_model_snapshot: Some(request.model_name.clone()),
            provider_system_fingerprint: None,
        },
        budget: EffectiveTokenBudget {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            budget_profile: overflow_recovery_profile("test-overflow-target-budget"),
            context_window_tokens: 1_047_576,
            requested_output_tokens: 32_768,
            safety_buffer_tokens: 8_192,
        },
    };
    proof.validate_for(
        frozen_request.fingerprint(),
        TokenMeasurementScope::RenderedTargetInput,
        &binding,
    )?;
    Ok(PortableTargetRequestMaterial::new(
        frozen_request,
        binding,
        proof,
    ))
}

#[async_trait]
impl Provider for OverflowRecoveryProvider {
    fn name(&self) -> &str {
        "openai_responses"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        PlannedProvider::new(Vec::new()).capabilities()
    }

    fn classify_pre_generation_rejection(
        &self,
        _error: &anyhow::Error,
    ) -> Option<ProviderRequestRejection> {
        Some(ProviderRequestRejection::ContextWindowExceeded)
    }

    async fn prove_portable_compaction_target(
        &self,
        frozen_request: FrozenProviderRequestMaterial,
    ) -> Result<PortableTargetRequestMaterial> {
        *self
            .target_proof_calls
            .lock()
            .expect("target proof mutex should not be poisoned") += 1;
        overflow_recovery_target_material(frozen_request)
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        *self
            .stream_calls
            .lock()
            .expect("stream call mutex should not be poisoned") += 1;
        let plan = self
            .plans
            .lock()
            .expect("plans mutex should not be poisoned")
            .pop_front()
            .unwrap_or(StreamPlan::Pending);
        match plan {
            StreamPlan::Chunks(chunks) => Ok(Box::pin(stream::iter(
                chunks.into_iter().map(Ok::<_, anyhow::Error>),
            ))),
            StreamPlan::Pending => Ok(Box::pin(stream::pending())),
            StreamPlan::Fail(error) => Err(anyhow!(error)),
        }
    }
}

fn seed_overflow_recovery_history(store: &JsonlSessionStore) -> Result<()> {
    store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "openai_responses".to_owned(),
        model_name: "gpt-4.1-2025-04-14".to_owned(),
    }))?;
    store.append(&SessionLogEntry::User(ModelMessage::user(
        "older user request",
    )))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("older assistant response".to_owned()),
        Vec::new(),
    )))?;
    store.append(&SessionLogEntry::User(ModelMessage::user(
        "retain this latest request",
    )))?;
    Ok(())
}

fn overflow_recovery_config(workspace_root: &Path) -> sigil_kernel::RootConfig {
    let mut config = test_root_config(workspace_root, "openai_responses", "gpt-4.1-2025-04-14");
    config.compaction.tail_messages = 1;
    config
}

#[test]
fn exact_overflow_rejection_does_not_apply_or_retry_while_v2_is_frozen() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-overflow-recovery.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    seed_overflow_recovery_history(&store)?;
    let provider = OverflowRecoveryProvider::new(vec![
        StreamPlan::Fail("exact context-window rejection"),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("recovered response".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let observed_provider = provider.clone();
    let worker = spawn_test_worker(
        overflow_recovery_config(&workspace_root),
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "new request that initially overflows".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let notice = worker.recv_until(|message| matches!(message, WorkerMessage::Notice(_)))?;
    assert!(matches!(
        notice,
        WorkerMessage::Notice(ref text)
            if text.contains("V2 context compaction apply is temporarily frozen")
    ));
    assert_eq!(observed_provider.target_proof_calls(), 0);
    assert_eq!(observed_provider.stream_calls(), 1);

    let records = JsonlSessionStore::read_event_records(&session_log_path)?;
    assert!(!records.iter().any(|record| {
        let event = record.stored_event();
        event.event_type == DurableEventType::ProviderPhysicalAttemptStarted.as_str()
            && event.payload["purpose"] == "input_token_measurement"
    }));
    assert!(!has_v2_compaction_lifecycle_event(&session_log_path)?);

    worker.shutdown()?;
    Ok(())
}

#[test]
fn frozen_overflow_recovery_is_not_recursively_retried() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-overflow-no-retry.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    seed_overflow_recovery_history(&store)?;
    let provider = OverflowRecoveryProvider::new(vec![
        StreamPlan::Fail("first exact context-window rejection"),
        StreamPlan::Fail("second exact context-window rejection"),
    ]);
    let observed_provider = provider.clone();
    let worker = spawn_test_worker(
        overflow_recovery_config(&workspace_root),
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "request that overflows twice".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let notice = worker.recv_until(|message| matches!(message, WorkerMessage::Notice(_)))?;
    assert!(
        matches!(
            notice,
            WorkerMessage::Notice(ref text)
                if text.contains("V2 context compaction apply is temporarily frozen")
        ),
        "overflow recovery did not report the activation freeze: {notice:?}"
    );
    assert_eq!(observed_provider.target_proof_calls(), 0);
    assert_eq!(observed_provider.stream_calls(), 1);
    assert!(!has_v2_compaction_lifecycle_event(&session_log_path)?);

    worker.shutdown()?;
    Ok(())
}

#[test]
fn compact_preview_is_read_only_and_reports_the_v2_fold_plan() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-compact.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.compaction.tail_messages = 2;
    let store = JsonlSessionStore::new(&session_log_path)?;
    store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "planned".to_owned(),
        model_name: "planned-model".to_owned(),
    }))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("one")))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("two".to_owned()),
        Vec::new(),
    )))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("three")))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("four".to_owned()),
        Vec::new(),
    )))?;
    let before = std::fs::read(&session_log_path)?;

    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::PreviewV2Compaction)?;
    let preview = worker
        .recv_until(|message| matches!(message, WorkerMessage::V2CompactionPreviewed { .. }))?;
    let WorkerMessage::V2CompactionPreviewed {
        state: V2CompactionPreviewState::Review(review),
    } = preview
    else {
        panic!("expected a V2 compaction preview with foldable history");
    };
    assert_eq!(review.preview.plan.folded_event_ids.len(), 2);
    assert_eq!(review.preview.plan.retained_event_ids.len(), 2);
    assert!(matches!(
        review.admission,
        V2CompactionAdmission::Unavailable { ref reason }
            if reason == "V2 context compaction apply is temporarily frozen while correctness fixes are in progress"
    ));
    assert_eq!(std::fs::read(&session_log_path)?, before);

    worker.shutdown()?;
    Ok(())
}

#[test]
fn compact_preview_is_rejected_while_run_is_active() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-compact-busy.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hold".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;

    worker.send(WorkerCommand::PreviewV2Compaction)?;
    let error = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    assert!(matches!(
        error,
        WorkerMessage::RunFailed(ref text)
            if text == "cannot preview compaction while the agent is running"
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn compact_preview_without_foldable_history_returns_an_empty_preview() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-compact-empty.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::PreviewV2Compaction)?;
    let preview = worker
        .recv_until(|message| matches!(message, WorkerMessage::V2CompactionPreviewed { .. }))?;
    assert!(matches!(
        preview,
        WorkerMessage::V2CompactionPreviewed {
            state: V2CompactionPreviewState::NoFoldableHistory {
                durable_message_count: 0,
                configured_tail_message_count: 6,
            },
        }
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn compact_preview_without_older_history_reports_message_count_and_raw_tail() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-compact-raw-tail.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    store.append(&SessionLogEntry::User(ModelMessage::user("first request")))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("first response".to_owned()),
        Vec::new(),
    )))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("second request")))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("second response".to_owned()),
        Vec::new(),
    )))?;

    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::PreviewV2Compaction)?;
    let preview = worker
        .recv_until(|message| matches!(message, WorkerMessage::V2CompactionPreviewed { .. }))?;
    assert!(matches!(
        preview,
        WorkerMessage::V2CompactionPreviewed {
            state: V2CompactionPreviewState::NoFoldableHistory {
                durable_message_count: 4,
                configured_tail_message_count: 6,
            },
        }
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn hard_threshold_idle_run_checks_local_admission_without_writing_an_unadmitted_lifecycle()
-> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-auto-compact.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.compaction.context_window_tokens = Some(100);
    root_config.compaction.soft_threshold_ratio = 0.5;
    root_config.compaction.hard_threshold_ratio = 0.8;
    root_config.compaction.tail_messages = 1;

    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::Usage(UsageStats {
            prompt_tokens: 90,
            completion_tokens: 12,
            cache_hit_tokens: 0,
            cache_miss_tokens: 90,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        }),
        ProviderChunk::TextDelta("finished turn".to_owned()),
        ProviderChunk::Done,
    ])]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hello".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    let notice = worker.recv_until(|message| {
        matches!(message, WorkerMessage::Notice(text) if text.contains("automatic compaction was not applied"))
    })?;
    assert!(matches!(
        notice,
        WorkerMessage::Notice(ref text)
            if text.contains("local target admission is unavailable")
    ));
    assert!(!has_v2_compaction_lifecycle_event(&session_log_path)?);

    worker.shutdown()?;
    Ok(())
}

#[test]
fn provider_context_window_prevents_early_auto_compaction() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-provider-window.jsonl");
    let mut root_config = test_root_config(&workspace_root, "deepseek", "deepseek-v4-pro");
    root_config.compaction.context_window_tokens = Some(128_000);
    root_config.compaction.soft_threshold_ratio = 0.5;
    root_config.compaction.hard_threshold_ratio = 0.8;
    root_config.compaction.tail_messages = 1;

    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::Usage(UsageStats {
            prompt_tokens: 90_354,
            completion_tokens: 12,
            cache_hit_tokens: 0,
            cache_miss_tokens: 90_354,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        }),
        ProviderChunk::TextDelta("finished turn".to_owned()),
        ProviderChunk::Done,
    ])]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hello".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    assert!(
        worker
            .recv_with_timeout(Duration::from_millis(150))
            .is_err()
    );
    assert!(!has_v2_compaction_lifecycle_event(&session_log_path)?);

    worker.shutdown()?;
    Ok(())
}
