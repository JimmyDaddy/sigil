use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};
use sigil_kernel::{
    Agent, COMPACTION_TOKEN_PROOF_SCHEMA_VERSION, CompletionRequest, ControlEntry,
    DurableEventType, EffectiveTokenBudget, FrozenProviderRequestMaterial, InputTokenEvidence,
    JsonlSessionStore, ModelMessage, PortableTargetRequestMaterial, Provider, ProviderCapabilities,
    ProviderChunk, ProviderRequestRejection, ReasoningEffort, RequestFitProof, Session,
    SessionLogEntry, TokenMeasurementBinding, TokenMeasurementScope, ToolRegistry,
    VersionedProfileIdentity,
};
use std::{
    collections::VecDeque,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tempfile::tempdir;

use super::{
    super::{
        V2CompactionPreviewState, WorkerCommand, WorkerMessage,
        worker_event::{WorkerEvent, WorkerEventPayloadSender},
    },
    common::{PlannedProvider, StreamPlan, spawn_test_worker, test_root_config},
};

fn compaction_result_channel() -> (
    WorkerEventPayloadSender<CompactionPreparationTaskResult>,
    std::sync::mpsc::Receiver<WorkerEvent>,
) {
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    (WorkerEventPayloadSender::compaction(event_tx), event_rx)
}

fn compaction_result(event: WorkerEvent) -> CompactionPreparationTaskResult {
    let WorkerEvent::CompactionPrepared(result) = event else {
        panic!("expected compaction preparation event");
    };
    result
}
use crate::runner::worker_loop::{
    CompactionPreparationTaskManager, CompactionPreparationTaskResult,
    IdleAutoCompactionPreparation, IdleAutoCompactionState, IdleV2CompactionPreparation,
    ManualV2CompactionPreparation, OverflowV2CompactionPreparation,
};

#[test]
fn replacement_compaction_preparation_cancels_and_discards_the_old_result() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let (result_tx, result_rx) = compaction_result_channel();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut tasks = CompactionPreparationTaskManager::new();

    tasks.start_manual(
        &runtime,
        41,
        "session-a".to_owned(),
        result_tx.clone(),
        move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
            Err::<ManualV2CompactionPreparation, _>("superseded".to_owned())
        },
    );
    started_rx.recv_timeout(Duration::from_secs(1))?;

    tasks.start_manual(&runtime, 42, "session-a".to_owned(), result_tx, || {
        Err::<ManualV2CompactionPreparation, _>("current".to_owned())
    });
    let current = compaction_result(result_rx.recv_timeout(Duration::from_secs(1))?);
    assert!(matches!(
        current,
        CompactionPreparationTaskResult::Manual {
            request_id: 42,
            ref session_scope_id,
            result: Err(ref error),
        } if session_scope_id == "session-a" && error == "current"
    ));
    assert!(tasks.accept_result(42, "session-a"));

    let _ = release_tx.send(());
    assert!(result_rx.recv_timeout(Duration::from_millis(100)).is_err());
    Ok(())
}

#[test]
fn idle_compaction_preparation_has_one_owned_background_result() -> Result<()> {
    let temp = tempdir()?;
    let session = Session::new("test", "model")
        .with_store(JsonlSessionStore::new(temp.path().join("session.jsonl"))?);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let (result_tx, result_rx) = compaction_result_channel();
    let mut tasks = CompactionPreparationTaskManager::new();

    tasks.start_idle(&runtime, 43, "session-idle".to_owned(), result_tx, || {
        Ok(IdleV2CompactionPreparation {
            state: IdleAutoCompactionState::default(),
            preparation: Ok(IdleAutoCompactionPreparation::NotRequested),
            session,
        })
    });
    assert!(tasks.has_active());
    let result = compaction_result(result_rx.recv_timeout(Duration::from_secs(1))?);
    let CompactionPreparationTaskResult::Idle {
        request_id,
        session_scope_id,
        result,
    } = result
    else {
        panic!("expected idle preparation result");
    };
    assert_eq!(request_id, 43);
    assert_eq!(session_scope_id, "session-idle");
    let prepared = result.map_err(anyhow::Error::msg)?;
    assert!(matches!(
        prepared.preparation,
        Ok(IdleAutoCompactionPreparation::NotRequested)
    ));
    assert!(tasks.accept_result(43, "session-idle"));
    assert!(!tasks.has_active());
    Ok(())
}

#[test]
fn cancelled_overflow_preparation_finishes_without_publishing_a_stale_result() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let (result_tx, result_rx) = compaction_result_channel();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut tasks = CompactionPreparationTaskManager::new();

    tasks.start_overflow(
        &runtime,
        44,
        "session-overflow".to_owned(),
        result_tx,
        move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
            Err::<OverflowV2CompactionPreparation, _>("cancelled".to_owned())
        },
    );
    started_rx.recv_timeout(Duration::from_secs(1))?;
    tasks.abort_all();
    let _ = release_tx.send(());
    assert!(result_rx.recv_timeout(Duration::from_millis(100)).is_err());
    Ok(())
}

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
    input_tokens: u64,
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
            tokens: input_tokens,
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
        let mut calls = self
            .target_proof_calls
            .lock()
            .expect("target proof mutex should not be poisoned");
        *calls += 1;
        let input_tokens = if *calls == 1 { 10_000 } else { 1 };
        overflow_recovery_target_material(frozen_request, input_tokens)
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        *self
            .stream_calls
            .lock()
            .expect("stream call mutex should not be poisoned") += 1;
        if let Some(instruction) = request
            .messages
            .last()
            .filter(|message| message.id.starts_with("semantic-compaction-instruction:"))
        {
            let content = instruction
                .content
                .as_deref()
                .context("semantic compaction instruction has no content")?;
            let (_, source_json) = content
                .split_once("SOURCE_INDEX=")
                .context("semantic compaction instruction has no source index")?;
            let source_index: Vec<serde_json::Value> = serde_json::from_str(source_json)?;
            let source_event_id = source_index
                .first()
                .and_then(|entry| entry.get("event_id"))
                .and_then(serde_json::Value::as_str)
                .context("semantic compaction source index is empty")?;
            let output = serde_json::json!({
                "in_progress": [{
                    "text": "Preserve the earlier technical context for the retry.",
                    "source_event_ids": [source_event_id],
                    "priority": "normal"
                }],
                "pending_actions": [],
                "provider_continuity": [],
                "model_notes": []
            })
            .to_string();
            return Ok(Box::pin(stream::iter(
                [ProviderChunk::TextDelta(output), ProviderChunk::Done]
                    .into_iter()
                    .map(Ok::<_, anyhow::Error>),
            )));
        }
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
            StreamPlan::GatedChunks { gate, chunks } => Ok(Box::pin(
                stream::once(async move {
                    gate.notified().await;
                    chunks
                })
                .flat_map(|chunks| stream::iter(chunks.into_iter().map(Ok::<_, anyhow::Error>))),
            )),
            StreamPlan::Pending => Ok(Box::pin(stream::pending())),
            StreamPlan::Fail(error) => Err(anyhow!(error)),
        }
    }
}

fn seed_overflow_recovery_history(store: &JsonlSessionStore) -> Result<()> {
    store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "openai_responses".to_owned(),
        model_name: "gpt-4.1-2025-04-14".to_owned(),
        resolved_model_route: None,
    }))?;
    for index in 0..6 {
        store.append(&SessionLogEntry::User(ModelMessage::user(format!(
            "older user request {index}: {}",
            "u".repeat(12_000)
        ))))?;
        store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
            Some(format!(
                "older assistant response {index}: {}",
                "a".repeat(12_000)
            )),
            Vec::new(),
        )))?;
    }
    store.append(&SessionLogEntry::User(ModelMessage::user(
        "retain this latest request",
    )))?;
    anyhow::ensure!(
        store
            .adaptive_compaction_preview(
                sigil_kernel::AdaptiveTailPolicyV3::default(),
                1_006_616,
                None,
            )?
            .is_some(),
        "overflow recovery fixture must contain foldable current-format history"
    );
    Ok(())
}

fn overflow_recovery_config(workspace_root: &Path) -> sigil_kernel::RootConfig {
    let mut config = test_root_config(workspace_root, "openai_responses", "gpt-4.1-2025-04-14");
    config.compaction.context_window_tokens = Some(1_047_576);
    config
}

#[test]
fn exact_overflow_rejection_applies_and_retries_once_with_owned_preparation() -> Result<()> {
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
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut notices = Vec::new();
    let applied = loop {
        let message =
            worker.recv_with_timeout(deadline.saturating_duration_since(Instant::now()))?;
        match message {
            WorkerMessage::V2CompactionApplied {
                source: super::super::V2CompactionApplySource::OverflowRecovery,
                ..
            } => break message,
            WorkerMessage::Notice(notice) => notices.push(notice),
            WorkerMessage::RunFailed(error) => {
                return Err(anyhow!(
                    "overflow recovery failed before apply: {error}; notices: {notices:?}"
                ));
            }
            _ => {}
        }
    };
    assert!(matches!(
        applied,
        WorkerMessage::V2CompactionApplied {
            source: super::super::V2CompactionApplySource::OverflowRecovery,
            ..
        }
    ));
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    assert_eq!(observed_provider.target_proof_calls(), 2);
    assert_eq!(observed_provider.stream_calls(), 3);

    let records = JsonlSessionStore::read_event_records(&session_log_path)?;
    assert_eq!(
        records
            .iter()
            .filter(|record| {
                let event = record.stored_event();
                event.event_type == DurableEventType::ProviderPhysicalAttemptStarted.as_str()
                    && event.payload["purpose"] == "input_token_measurement"
            })
            .count(),
        2
    );
    assert!(has_v2_compaction_lifecycle_event(&session_log_path)?);

    worker.shutdown()?;
    Ok(())
}

#[test]
fn overflow_recovery_is_not_recursively_retried() -> Result<()> {
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
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut notices = Vec::new();
    loop {
        let message =
            worker.recv_with_timeout(deadline.saturating_duration_since(Instant::now()))?;
        match message {
            WorkerMessage::V2CompactionApplied {
                source: super::super::V2CompactionApplySource::OverflowRecovery,
                ..
            } => break,
            WorkerMessage::Notice(notice) => notices.push(notice),
            WorkerMessage::RunFailed(error) => {
                return Err(anyhow!(
                    "overflow recovery failed before apply: {error}; notices: {notices:?}"
                ));
            }
            _ => {}
        }
    }
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;
    assert_eq!(observed_provider.target_proof_calls(), 2);
    assert_eq!(observed_provider.stream_calls(), 3);
    assert!(has_v2_compaction_lifecycle_event(&session_log_path)?);

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
                minimum_tail_turn_count: 2,
            },
        }
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn compact_preview_without_older_history_reports_message_count_and_minimum_tail() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-compact-raw-tail.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    session.append_user_message(ModelMessage::user("first request"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("first response".to_owned()),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user("second request"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("second response".to_owned()),
        Vec::new(),
    ))?;
    drop(session);

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
                minimum_tail_turn_count: 2,
            },
        }
    ));

    worker.shutdown()?;
    Ok(())
}
