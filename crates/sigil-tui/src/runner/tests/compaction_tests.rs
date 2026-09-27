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

fn compaction_test_attachment() -> Result<(
    tempfile::TempDir,
    Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
)> {
    let temp = tempdir()?;
    let attachment = Arc::new(
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            temp.path().join("session.jsonl"),
        )?,
    );
    Ok((temp, attachment))
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
    let (_attachment_temp, attachment) = compaction_test_attachment()?;

    tasks
        .start_manual(
            &runtime,
            41,
            "session-a".to_owned(),
            Arc::clone(&attachment),
            result_tx.clone(),
            move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                Err::<ManualV2CompactionPreparation, _>("superseded".to_owned())
            },
        )
        .map_err(anyhow::Error::msg)?;
    started_rx.recv_timeout(Duration::from_secs(1))?;

    tasks
        .start_manual(
            &runtime,
            42,
            "session-a".to_owned(),
            attachment,
            result_tx,
            || Err::<ManualV2CompactionPreparation, _>("current".to_owned()),
        )
        .map_err(anyhow::Error::msg)?;
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
    let (_attachment_temp, attachment) = compaction_test_attachment()?;

    tasks
        .start_idle(
            &runtime,
            43,
            "session-idle".to_owned(),
            attachment,
            result_tx,
            || {
                Ok(IdleV2CompactionPreparation {
                    state: IdleAutoCompactionState::default(),
                    preparation: Ok(IdleAutoCompactionPreparation::NotRequested),
                    session,
                })
            },
        )
        .map_err(anyhow::Error::msg)?;
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
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while tasks.has_active() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
        tasks.reap_finished();
    }
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
    let (_attachment_temp, attachment) = compaction_test_attachment()?;

    tasks
        .start_overflow(
            &runtime,
            44,
            "session-overflow".to_owned(),
            attachment,
            result_tx,
            move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                Err::<OverflowV2CompactionPreparation, _>("cancelled".to_owned())
            },
        )
        .map_err(anyhow::Error::msg)?;
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
    provider_name: &'static str,
    before_tokens: u64,
    plans: Arc<Mutex<VecDeque<StreamPlan>>>,
    stream_calls: Arc<Mutex<usize>>,
    target_proof_calls: Arc<Mutex<usize>>,
}

impl OverflowRecoveryProvider {
    fn new(plans: Vec<StreamPlan>) -> Self {
        Self {
            provider_name: "openai_responses",
            before_tokens: 10_000,
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
    role: sigil_kernel::provider::PortableCompactionRequestRole,
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
    proof.input.validate_for(
        frozen_request.fingerprint(),
        TokenMeasurementScope::RenderedTargetInput,
        &binding,
    )?;
    proof.budget.validate()?;
    if role == sigil_kernel::provider::PortableCompactionRequestRole::Target {
        proof.validate_for(
            frozen_request.fingerprint(),
            TokenMeasurementScope::RenderedTargetInput,
            &binding,
        )?;
    }
    Ok(PortableTargetRequestMaterial::new(
        frozen_request,
        binding,
        proof,
    ))
}

#[async_trait]
impl Provider for OverflowRecoveryProvider {
    fn name(&self) -> &str {
        self.provider_name
    }

    fn capabilities(&self) -> ProviderCapabilities {
        PlannedProvider::new(Vec::new()).capabilities()
    }

    fn context_capabilities(&self, _model_name: &str) -> sigil_kernel::ProviderContextCapabilities {
        sigil_kernel::ProviderContextCapabilities {
            cache_mode: sigil_kernel::CacheMode::ImplicitPrefix,
            ..sigil_kernel::ProviderContextCapabilities::default()
        }
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
        role: sigil_kernel::provider::PortableCompactionRequestRole,
    ) -> Result<PortableTargetRequestMaterial> {
        let mut calls = self
            .target_proof_calls
            .lock()
            .expect("target proof mutex should not be poisoned");
        *calls += 1;
        let input_tokens = if *calls == 1 { self.before_tokens } else { 1 };
        overflow_recovery_target_material(frozen_request, input_tokens, role)
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
                [
                    ProviderChunk::TextDelta(output),
                    ProviderChunk::Usage(sigil_kernel::UsageStats {
                        prompt_tokens: self.before_tokens,
                        completion_tokens: 64,
                        cache_miss_tokens: self.before_tokens,
                        ..Default::default()
                    }),
                    ProviderChunk::Done,
                ]
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

fn seed_overflow_recovery_history(
    store: &JsonlSessionStore,
    root_config: &sigil_kernel::RootConfig,
) -> Result<()> {
    seed_remote_compaction_history(store, root_config, "openai_responses", "gpt-4.1-2025-04-14")
}

fn seed_remote_compaction_history(
    store: &JsonlSessionStore,
    root_config: &sigil_kernel::RootConfig,
    provider_name: &str,
    model_name: &str,
) -> Result<()> {
    store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: provider_name.to_owned(),
        model_name: model_name.to_owned(),
        resolved_model_route: None,
    }))?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::SessionCompositionBound(sigil_kernel::SessionCompositionSnapshotV1::new(
            root_config.selected_capabilities(),
        )),
    ))?;
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

fn assert_overflow_public_run_boundaries(
    records: &[sigil_kernel::SessionStreamRecord],
    recovery_terminal_status: sigil_kernel::ConversationRunTerminalStatusV1,
) -> Result<()> {
    let attempts = sigil_kernel::ProviderPhysicalAttemptProjection::from_records(records)?;
    let provider_recoveries = sigil_kernel::ProviderTurnRecoveryProjection::from_records(records)?;
    let conversation_attempts = attempts
        .attempts()
        .into_iter()
        .filter(|attempt| {
            attempt.entry.purpose
                == sigil_kernel::ProviderPhysicalAttemptPurpose::ConversationGeneration
        })
        .collect::<Vec<_>>();
    assert_eq!(conversation_attempts.len(), 2);
    let original = conversation_attempts[0];
    let recovery = conversation_attempts[1];
    assert_eq!(
        recovery.entry.logical_run_id,
        format!("overflow-recovery-{}", original.entry.physical_attempt_id)
    );
    assert_ne!(original.entry.logical_run_id, recovery.entry.logical_run_id);
    assert_ne!(
        original.entry.request_material_fingerprint, recovery.entry.request_material_fingerprint,
        "portable compaction creates a new frozen request with its own logical provider identity"
    );

    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(records)?;
    let public = outbox.events_in_order();
    let mut run_boundaries = Vec::new();
    for (attempt, expected_status) in [
        (
            original,
            sigil_kernel::ConversationRunTerminalStatusV1::Blocked,
        ),
        (recovery, recovery_terminal_status),
    ] {
        let run_id = attempt.entry.logical_run_id.as_str();
        assert!(
            attempts
                .effective_attempt_for_logical_run_id(run_id)?
                .is_some()
        );
        if expected_status == sigil_kernel::ConversationRunTerminalStatusV1::Blocked {
            // Exact capacity rejection requires a new request; it is a typed blocked
            // provider turn, even when the legacy worker reports RunFailed.
            let physical_terminal = attempt
                .terminal
                .as_ref()
                .context("blocked overflow attempt must have a durable physical terminal")?;
            assert_eq!(
                physical_terminal.outcome,
                sigil_kernel::ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption
            );
            assert_eq!(
                physical_terminal.rejection,
                Some(ProviderRequestRejection::ContextWindowExceeded)
            );
            let recovery_terminal = provider_recoveries
                .terminal_for_logical_run_id(run_id)
                .context("overflow rejection must retain its typed recovery disposition")?;
            assert_eq!(
                recovery_terminal.last_physical_attempt_id,
                attempt.entry.physical_attempt_id
            );
            assert_eq!(
                recovery_terminal.terminal_disposition,
                sigil_kernel::ProviderTurnRecoveryTerminalDispositionV1::Blocked
            );
            assert_eq!(
                recovery_terminal.reason_code,
                "provider_configuration_or_capacity_required"
            );
        }
        let events = public
            .iter()
            .filter(|entry| entry.run_id == run_id)
            .collect::<Vec<_>>();
        assert!(!events.is_empty());
        for (index, entry) in events.iter().enumerate() {
            assert_eq!(entry.sequence, index as u64 + 1);
        }
        assert_eq!(
            events
                .iter()
                .filter(|entry| matches!(
                    entry.event.event,
                    sigil_kernel::PublicRunEventKind::RunStarted { .. }
                ))
                .count(),
            1,
            "each provider logical run publishes exactly one RunStarted"
        );
        assert!(matches!(
            events.first().map(|entry| &entry.event.event),
            Some(sigil_kernel::PublicRunEventKind::RunStarted { .. })
        ));
        let terminals = records
            .iter()
            .map(sigil_kernel::conversation_run_lifecycle_record_from_stream)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .filter_map(|record| match record {
                sigil_kernel::ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(
                    terminal,
                ) if terminal.run_id() == run_id => Some(terminal),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals[0].status(), expected_status);
        assert_eq!(
            events
                .iter()
                .filter(|entry| matches!(
                    entry.event.event,
                    sigil_kernel::PublicRunEventKind::RunFinished { .. }
                        | sigil_kernel::PublicRunEventKind::RunBlocked { .. }
                        | sigil_kernel::PublicRunEventKind::RunFailed { .. }
                ))
                .count(),
            1
        );
        let terminal = events.last().expect("run public events are present");
        assert!(match &terminal.event.event {
            sigil_kernel::PublicRunEventKind::RunFinished { .. } => {
                expected_status == sigil_kernel::ConversationRunTerminalStatusV1::Succeeded
            }
            sigil_kernel::PublicRunEventKind::RunBlocked { .. } => {
                expected_status == sigil_kernel::ConversationRunTerminalStatusV1::Blocked
            }
            _ => false,
        });
        let start_id = &events[0].public_event_id;
        let terminal_id = &terminal.public_event_id;
        let start_sequence = records
            .iter()
            .map(sigil_kernel::SessionStreamRecord::stored_event)
            .find(|event| &event.event_id == start_id)
            .expect("public start has a durable envelope")
            .stream_sequence;
        let terminal_sequence = records
            .iter()
            .map(sigil_kernel::SessionStreamRecord::stored_event)
            .find(|event| &event.event_id == terminal_id)
            .expect("public terminal has a durable envelope")
            .stream_sequence;
        run_boundaries.push((start_sequence, terminal_sequence));
    }
    assert!(
        run_boundaries[0].1 < run_boundaries[1].0,
        "the rejected run is terminal before the separately owned retry starts"
    );
    Ok(())
}

#[test]
fn remote_messages_pressure_compacts_through_the_actual_worker_and_reopens() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/messages-pressure.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut root_config = test_root_config(&workspace_root, "anthropic", "deepseek-flash");
    root_config.compaction.context_window_tokens = Some(1_047_576);
    seed_remote_compaction_history(&store, &root_config, "anthropic", "deepseek-flash")?;
    let mut provider = OverflowRecoveryProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::TextDelta("completed turn before idle pressure check".to_owned()),
        ProviderChunk::Usage(sigil_kernel::UsageStats {
            prompt_tokens: 1_030_000,
            completion_tokens: 10,
            cache_miss_tokens: 1_030_000,
            ..Default::default()
        }),
        ProviderChunk::Done,
    ])]);
    provider.provider_name = "anthropic";
    provider.before_tokens = 1_030_000;
    let observed_provider = provider.clone();
    let worker = spawn_test_worker(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
    )?;
    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "finish the current observation".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut notices = Vec::new();
    loop {
        let message = worker
            .recv_with_timeout(deadline.saturating_duration_since(Instant::now()))
            .with_context(|| format!("idle pressure did not apply; notices: {notices:?}"))?;
        match message {
            WorkerMessage::V2CompactionApplied {
                source: super::super::V2CompactionApplySource::IdleAutomatic,
                ..
            } => break,
            WorkerMessage::Notice(notice) => notices.push(notice),
            WorkerMessage::RunFailed(error) => anyhow::bail!("pressure run failed: {error}"),
            _ => {}
        }
    }
    worker.shutdown()?;
    assert_eq!(
        observed_provider.stream_calls(),
        2,
        "one ordinary turn and one summary"
    );
    assert_eq!(
        observed_provider.target_proof_calls(),
        2,
        "one before and one target measurement"
    );
    let restored = Session::load_from_store("anthropic", "deepseek-flash", store)?;
    let projection = restored
        .active_projection_snapshot()?
        .context("durable projection")?;
    assert!(
        projection
            .compaction()
            .latest_applied_compaction_id()
            .is_some()
    );
    assert_eq!(projection.compaction().open_attempt_count(), 0);
    Ok(())
}

#[test]
fn exact_overflow_rejection_applies_and_retries_once_with_owned_preparation() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-overflow-recovery.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let root_config = overflow_recovery_config(&workspace_root);
    seed_overflow_recovery_history(&store, &root_config)?;
    let provider = OverflowRecoveryProvider::new(vec![
        StreamPlan::Fail("exact context-window rejection"),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("recovered response".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let observed_provider = provider.clone();
    let worker = spawn_test_worker(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "new request that initially overflows".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    // Hosted qualification runners can briefly contend for worker-thread and
    // filesystem resources; keep the assertion bounded without treating that
    // contention as a compaction protocol failure.
    let deadline = Instant::now() + Duration::from_secs(30);
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
    assert_overflow_public_run_boundaries(
        &records,
        sigil_kernel::ConversationRunTerminalStatusV1::Succeeded,
    )?;
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
    let root_config = overflow_recovery_config(&workspace_root);
    seed_overflow_recovery_history(&store, &root_config)?;
    let provider = OverflowRecoveryProvider::new(vec![
        StreamPlan::Fail("first exact context-window rejection"),
        StreamPlan::Fail("second exact context-window rejection"),
    ]);
    let observed_provider = provider.clone();
    let worker = spawn_test_worker(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "request that overflows twice".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    // Keep this worker-event wait bounded while allowing the hosted Linux
    // qualification runner's transient resource contention to settle.
    let deadline = Instant::now() + Duration::from_secs(30);
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
    assert_overflow_public_run_boundaries(
        &JsonlSessionStore::read_event_records(&session_log_path)?,
        sigil_kernel::ConversationRunTerminalStatusV1::Blocked,
    )?;

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
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    sigil_runtime::bind_session_composition(&mut session, &root_config)?;
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
