use std::{fs, time::Instant};

use anyhow::Result;

use super::*;
use crate::{
    COMPACTION_TOKEN_PROOF_SCHEMA_VERSION, CompletionRequest, ContextSensitivity,
    ContextTrustLevel, ContinuationCheckpointV1, EffectiveTokenBudget, ExternalProvenanceEntry,
    ExternalTrust, FrozenProviderRequestMaterial, InputTokenEvidence, ModelMessage,
    PortableTargetRequestMaterial, ProviderNonGeneratingAttempt,
    ProviderNonGeneratingAttemptReceipt, ProviderPhysicalAttemptOutcome,
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

#[test]
fn adaptive_checkpoint_rebuilds_the_same_whole_turn_plan_during_activation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("adaptive-activation.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    for index in 0..5 {
        session.append_user_message(ModelMessage::user(format!("adaptive user turn {index}")))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some(format!("adaptive assistant turn {index}")),
            Vec::new(),
        ))?;
    }
    let records = store.read_event_records_writer()?;
    let policy = AdaptiveTailPolicyV3 {
        tail_target_min_tokens: 1,
        tail_target_max_tokens: 64,
        ..AdaptiveTailPolicyV3::from_legacy_tail_messages(6)
    };
    let plan =
        CompactionFoldPlan::from_records_after_adaptive_tail(&records, policy, 900_000, None)?;
    let source_event_id = plan
        .folded_event_ids
        .first()
        .cloned()
        .expect("adaptive fixture has foldable history");
    let request = PortableSemanticCompactionRequest {
        attempt_id: "adaptive-activation-attempt".to_owned(),
        compaction_id: "adaptive-activation-compaction".to_owned(),
        initiation: CompactionInitiation::Manual,
        base_projection_revision: "adaptive-whole-turn-r1".to_owned(),
        branch_id: None,
        valid_for_snapshot: "snapshot-v1".to_owned(),
        objective: Some("Activate one adaptive whole-turn checkpoint".to_owned()),
        language: "en".to_owned(),
        plan,
        model_output: ContinuationModelOutputV1 {
            in_progress: vec![ContinuationModelOutputItemV1 {
                text: "Adaptive activation is in progress.".to_owned(),
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
    };
    let session_scope_id = session_scope_id(&store)?;
    execute_with_target(&store, request, |checkpoint, task_memory, candidate| {
        target_material(&session_scope_id, checkpoint, task_memory, candidate)
    })?;
    let records = store.read_event_records_writer()?;
    let sidecars = CompactionSidecarProjection::from_records(&records)?;
    let active = sidecars
        .latest_for_branch(None)
        .expect("adaptive checkpoint is active");
    assert!(active.checkpoint.adaptive_tail.is_some());
    Ok(())
}

#[test]
#[ignore = "release-profile long-session performance evidence"]
fn portable_compaction_long_session_evidence() -> Result<()> {
    const TURN_COUNT: usize = 1_000;

    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    for index in 0..TURN_COUNT {
        session.append_user_message(ModelMessage::user(format!("user turn {index}")))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some(format!("assistant turn {index}")),
            Vec::new(),
        ))?;
    }

    let records = store.read_event_records_writer()?;
    let file_bytes_before = fs::metadata(store.path())?.len();
    let planning_started = Instant::now();
    let plan = CompactionFoldPlan::from_records_after(&records, 1, None)?;
    let elapsed_ms = planning_started.elapsed().as_millis();
    let file_bytes_after = fs::metadata(store.path())?.len();

    assert!(!plan.folded_event_ids.is_empty());
    assert!(plan.folded_event_ids.len() < records.len());
    assert!(plan.folded_through.is_some());
    assert_eq!(file_bytes_after, file_bytes_before);

    println!(
        "SIGIL_LONG_SESSION_EVIDENCE {}",
        serde_json::json!({
            "schema_version": 1,
            "scenario": "portable_compaction_1k_turns",
            "scale": TURN_COUNT,
            "elapsed_ms": elapsed_ms,
            "facts": {
                "source_record_count": records.len(),
                "folded_event_count": plan.folded_event_ids.len(),
                "raw_file_bytes_before": file_bytes_before,
                "raw_file_bytes_after": file_bytes_after,
            }
        })
    );
    Ok(())
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

async fn record_completed_input_measurement(
    session: &Session,
    logical_run_id: &str,
    frozen_request: &FrozenProviderRequestMaterial,
) -> Result<ProviderNonGeneratingAttemptReceipt> {
    let mut measurement = ProviderNonGeneratingAttempt::start(
        session,
        logical_run_id,
        frozen_request,
        ProviderPhysicalAttemptPurpose::InputTokenMeasurement,
    )
    .await?;
    measurement
        .finish(session, ProviderPhysicalAttemptOutcome::Completed)
        .await?;
    measurement
        .completed_receipt()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("store-backed measurement has no completed receipt"))
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
    let receipt = runtime.block_on(record_completed_input_measurement(
        &session,
        "input-token-measurement-portable-test",
        &frozen_request,
    ))?;
    preflight.admit_completed_input_token_measurement(receipt, frozen_request.fingerprint())?;

    let outcome = store.execute_portable_semantic_compaction(preflight, target)?;
    assert_eq!(outcome.compaction_id, "compaction-measured");
    Ok(())
}

#[test]
fn overflow_portable_executor_admits_exact_before_and_after_measurements() -> Result<()> {
    let (_temp, store, session) = setup_session()?;
    let session_scope_id = session_scope_id(&store)?;
    let mut request = request(&store, "attempt-overflow", "compaction-overflow", None)?;
    request.initiation = CompactionInitiation::OverflowRecovery {
        source_physical_attempt_id: "rejected-attempt".to_owned(),
    };
    let mut preflight = store.prepare_portable_semantic_compaction(request)?;
    let target = target_material(
        &session_scope_id,
        preflight.checkpoint(),
        preflight.task_memory(),
        preflight.candidate_messages(),
    )?;
    let frozen_target_request = target.frozen_request.clone();
    let mut before_request = frozen_target_request.request().clone();
    before_request
        .messages
        .push(ModelMessage::user("uncompacted overflow source material"));
    let frozen_before_request =
        FrozenProviderRequestMaterial::freeze(&session_scope_id, before_request)?;
    let before_input = InputTokenEvidence::Exact {
        tokens: 80,
        material_fingerprint: frozen_before_request.fingerprint().to_owned(),
        measurement_scope: TokenMeasurementScope::RenderedTargetInput,
        binding: target.binding.clone(),
        provider_model_snapshot: None,
        provider_system_fingerprint: None,
    };
    let target = PortableTargetRequestMaterial::new(
        frozen_target_request.clone(),
        target.binding,
        target.proof,
    )
    .with_portable_economics(&frozen_before_request, before_input)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    for (logical_run_id, frozen_request) in [
        ("overflow-before-count", &frozen_before_request),
        ("overflow-target-count", &frozen_target_request),
    ] {
        let receipt = runtime.block_on(record_completed_input_measurement(
            &session,
            logical_run_id,
            frozen_request,
        ))?;
        preflight.admit_completed_input_token_measurement(receipt, frozen_request.fingerprint())?;
    }

    let outcome = store.execute_portable_semantic_compaction(preflight, target)?;
    assert_eq!(outcome.compaction_id, "compaction-overflow");
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
fn idle_auto_compaction_preserves_task_list_memory_and_executable_projection() -> Result<()> {
    let (temp, store, mut session) = setup_session()?;
    let task_id = crate::TaskId::new("task-compact-survival")?;
    let inspect_step_id = crate::TaskStepId::new("inspect")?;
    let implement_step_id = crate::TaskStepId::new("implement")?;
    let admission = crate::admit_user_declared_root(
        &crate::IntentAdmissionContextV1::initial(
            crate::IntentStackId::new("stack-compact-survival")?,
            crate::stable_workspace_id(temp.path())?,
            session.session_scope_id(),
        )?,
        crate::UserDeclaredIntentV1 {
            title: "Preserve the accepted task and Intent".to_owned(),
            statement: "Retain executable task and Intent identity across automatic compaction."
                .to_owned(),
            acceptance_criteria: vec![crate::IntentProposalCriterionV1 {
                criterion_alias: "compact-survival".to_owned(),
                statement: "Task and Intent projections reload from the durable source.".to_owned(),
                required: true,
            }],
        },
        &crate::IntentAcceptanceAuthorityV1::user_declared_root(
            "turn-compact-survival",
            "event-compact-survival",
        )?,
    )?;
    let accepted_intent_ref = admission.plan().intents[0].intent_ref.clone();
    let task_plan = crate::bind_task_plan_intents(
        &admission,
        crate::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 3,
            status: crate::TaskPlanStatus::Accepted,
            steps: vec![
                crate::TaskStepSpec {
                    step_id: inspect_step_id.clone(),
                    title: "Inspect durable task controls".to_owned(),
                    display_name: Some("explorer".to_owned()),
                    detail: Some("Confirm the append-only source of truth".to_owned()),
                    role: crate::AgentRole::SubagentRead,
                    depends_on: Vec::new(),
                    intent_refs: Vec::new(),
                    mode: Some(crate::TaskStepMode::Read),
                    isolation: Some(crate::TaskIsolationMode::SharedReadOnly),
                },
                crate::TaskStepSpec {
                    step_id: implement_step_id.clone(),
                    title: "Continue the isolated implementation".to_owned(),
                    display_name: Some("implementer".to_owned()),
                    detail: Some("Resume only after inspection completes".to_owned()),
                    role: crate::AgentRole::SubagentWrite,
                    depends_on: vec![inspect_step_id.clone()],
                    intent_refs: Vec::new(),
                    mode: Some(crate::TaskStepMode::Write),
                    isolation: Some(crate::TaskIsolationMode::Worktree),
                },
            ],
            reason: None,
        },
        &[
            crate::TaskStepIntentAliasBindingV1 {
                step_id: inspect_step_id.clone(),
                intent_aliases: vec![crate::USER_DECLARED_ROOT_INTENT_ALIAS.to_owned()],
            },
            crate::TaskStepIntentAliasBindingV1 {
                step_id: implement_step_id.clone(),
                intent_aliases: vec![crate::USER_DECLARED_ROOT_INTENT_ALIAS.to_owned()],
            },
        ],
    )?;
    session.append_control(crate::ControlEntry::TaskRun(crate::TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: crate::SessionRef::new_relative("session-parent.jsonl")?,
        objective: "Preserve the active task across automatic compaction".to_owned(),
        status: crate::TaskRunStatus::Paused,
        reason: Some("waiting for continuation".to_owned()),
    }))?;
    crate::append_task_intent_plan_admission(&mut session, &admission, task_plan)?;
    session.append_controls(vec![
        crate::ControlEntry::TaskStep(crate::TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 3,
            step_id: inspect_step_id.clone(),
            role: crate::AgentRole::SubagentRead,
            status: crate::TaskStepStatus::Completed,
            title: Some("Inspect durable task controls".to_owned()),
            summary: Some("Task controls remain append-only".to_owned()),
            reason: None,
        }),
        crate::ControlEntry::TaskStep(crate::TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 3,
            step_id: implement_step_id.clone(),
            role: crate::AgentRole::SubagentWrite,
            status: crate::TaskStepStatus::Pending,
            title: Some("Continue the isolated implementation".to_owned()),
            summary: None,
            reason: Some("dependency completed; awaiting continue".to_owned()),
        }),
    ])?;

    let session_scope_id = session_scope_id(&store)?;
    let mut request = request(
        &store,
        "idle-task-survival-attempt",
        "idle-task-survival-compaction",
        None,
    )?;
    request.initiation = CompactionInitiation::IdleAutomatic {
        scope_fingerprint: "idle-task-survival-scope".to_owned(),
        circuit_scope: None,
    };
    let task_control_event_ids = store
        .read_event_records_writer()?
        .into_iter()
        .filter(|record| {
            record.stored_event().event_kind() == Some(DurableEventType::TaskStatusChanged)
        })
        .map(|record| record.event_id().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(task_control_event_ids.len(), 4);
    for event_id in &task_control_event_ids {
        assert!(
            request.plan.protected_events.iter().any(|protected| {
                protected.event.event_id == *event_id
                    && protected.reason == CompactionFoldProtectionReason::ControlState
            }),
            "task control {event_id} must never be folded"
        );
    }
    let intent_event_ids = store
        .read_event_records_writer()?
        .into_iter()
        .filter(|record| {
            matches!(
                record.stored_event().event_kind(),
                Some(
                    DurableEventType::IntentStackCreated
                        | DurableEventType::IntentPlanRecorded
                        | DurableEventType::IntentPlanAccepted
                )
            )
        })
        .map(|record| record.event_id().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(intent_event_ids.len(), 3);
    for event_id in &intent_event_ids {
        assert!(
            request.plan.protected_events.iter().any(|protected| {
                protected.event.event_id == *event_id
                    && protected.reason == CompactionFoldProtectionReason::NonMessageDurableEvent
            }),
            "Intent event {event_id} must never be folded"
        );
    }

    execute_with_target(&store, request, |checkpoint, task_memory, candidate| {
        target_material(&session_scope_id, checkpoint, task_memory, candidate)
    })?;

    let reloaded = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    let context = reloaded
        .try_context_projection_from_durable()?
        .expect("store-backed session has a durable context projection");
    let active_plan = context
        .task_memory
        .as_ref()
        .and_then(|memory| memory.active_plan.as_ref())
        .expect("automatic compaction should retain an active plan for model continuation");
    assert_eq!(active_plan.task_id, task_id.as_str());
    assert_eq!(active_plan.plan_version, 3);
    assert_eq!(active_plan.steps.len(), 2);
    assert_eq!(
        active_plan.steps[0].status,
        crate::TaskStepStatus::Completed
    );
    assert_eq!(active_plan.steps[1].status, crate::TaskStepStatus::Pending);
    let model_messages = context.model_messages();
    let checkpoint = model_messages
        .first()
        .and_then(|message| message.content.as_deref())
        .expect("portable checkpoint should be the first model-visible message");
    assert!(checkpoint.contains("Active Plan"));
    assert!(checkpoint.contains("[Completed] Inspect durable task controls"));
    assert!(checkpoint.contains("[Pending] Continue the isolated implementation"));

    let task_projection = reloaded
        .try_task_state_projection_from_durable()?
        .expect("store-backed session has a durable task projection");
    let task = task_projection
        .tasks
        .get(&task_id)
        .expect("active task should remain available for continuation");
    assert_eq!(task.status, crate::TaskRunStatus::Paused);
    let plan = task
        .plans
        .get(&3)
        .expect("accepted executable plan should survive compaction");
    assert_eq!(plan.steps.len(), 2);
    assert_eq!(plan.steps[1].depends_on, vec![inspect_step_id]);
    assert_eq!(plan.steps[1].role, crate::AgentRole::SubagentWrite);
    assert_eq!(plan.steps[1].mode, Some(crate::TaskStepMode::Write));
    assert_eq!(
        plan.steps[1].isolation,
        Some(crate::TaskIsolationMode::Worktree)
    );
    assert_eq!(
        plan.steps[1].intent_refs,
        vec![accepted_intent_ref.clone()],
        "Task to Intent binding must survive automatic compaction"
    );
    assert_eq!(
        task.steps
            .get(&(3, implement_step_id))
            .map(|step| step.status),
        Some(crate::TaskStepStatus::Pending)
    );
    assert_eq!(
        reloaded.task_state_projection(),
        task_projection,
        "the reloaded entry list forwarded to TUI must match durable Task replay"
    );
    let intent_state = reloaded.public_intent_stack_state_for_workspace(temp.path())?;
    let crate::PublicIntentStackStateV1::Available { stack, .. } = intent_state else {
        panic!("accepted Intent history must survive automatic compaction");
    };
    assert_eq!(stack.intents.len(), 1);
    assert_eq!(stack.intents[0].intent_ref, accepted_intent_ref);
    Ok(())
}

#[test]
fn portable_executor_persists_idle_auto_initiation_for_failure_latch_replay() -> Result<()> {
    let (_temp, store, _session) = setup_session()?;
    let session_scope_id = session_scope_id(&store)?;
    let mut request = request(&store, "idle-attempt", "idle-compaction", None)?;
    request.initiation = CompactionInitiation::IdleAutomatic {
        scope_fingerprint: "idle-scope-v1".to_owned(),
        circuit_scope: None,
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
        CompactionInitiation::IdleAutomatic {
            ref scope_fingerprint,
            ..
        }
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
    let third_continuity = active
        .checkpoint
        .continuity_v2
        .as_ref()
        .expect("portable compaction records continuity V2");
    let second = sidecars
        .resolved_compaction("compaction-2")
        .expect("second compaction remains auditable");
    assert_eq!(
        third_continuity.previous_checkpoint_id.as_deref(),
        second
            .checkpoint
            .continuity_v2
            .as_ref()
            .map(|continuity| continuity.checkpoint_id.as_str())
    );
    assert_eq!(
        active
            .checkpoint
            .session_anchor
            .as_ref()
            .expect("portable compaction records a session anchor")
            .root_objective
            .exact_text,
        "必须保留 CJK 约束：不要删除原始 JSONL，也不要使用旧日志 bridge。"
    );
    Ok(())
}

#[test]
fn repeated_compaction_preserves_accepted_constraint_and_honors_durable_supersession() -> Result<()>
{
    let (temp, store, mut session) = setup_session()?;
    let session_scope_id = session_scope_id(&store)?;
    let root_source_turn = session.messages()[0].id.clone();
    let root = crate::admit_user_declared_root(
        &crate::IntentAdmissionContextV1::initial(
            crate::IntentStackId::new("stack-continuity-v2")?,
            crate::stable_workspace_id(temp.path())?,
            session.session_scope_id(),
        )?,
        crate::UserDeclaredIntentV1 {
            title: "Implement portable continuity".to_owned(),
            statement: "Implement portable continuity without losing accepted authority."
                .to_owned(),
            acceptance_criteria: vec![crate::IntentProposalCriterionV1 {
                criterion_alias: "delivery-boundary".to_owned(),
                statement: "Do not commit or push.".to_owned(),
                required: true,
            }],
        },
        &crate::IntentAcceptanceAuthorityV1::user_declared_root(
            root_source_turn,
            "authority-continuity-v1",
        )?,
    )?;
    crate::append_chat_root_intent_admission(&session, &root)?;

    execute_with_target(
        &store,
        request(&store, "anchor-attempt-1", "anchor-compaction-1", None)?,
        |checkpoint, task_memory, candidate| {
            target_material(&session_scope_id, checkpoint, task_memory, candidate)
        },
    )?;

    let replacement_message =
        ModelMessage::user("Commits are allowed now, but do not push under any circumstance.");
    let replacement_turn_id = replacement_message.id.clone();
    session.append_user_message(replacement_message)?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("I will retain the new accepted delivery boundary.".to_owned()),
        Vec::new(),
    ))?;

    let mut successor_plan = root.plan().clone();
    successor_plan.stack_version = crate::IntentStackVersion::new(2)?;
    let previous_ref = successor_plan.intents[0].intent_ref.clone();
    successor_plan.intents[0].intent_ref =
        crate::IntentVersionRef::new(previous_ref.intent_id.clone(), 2)?;
    successor_plan.intents[0].statement =
        "Implement portable continuity without losing accepted authority.".to_owned();
    successor_plan.intents[0].acceptance_criteria[0].statement =
        "Commits are allowed, but do not push.".to_owned();
    successor_plan.intents[0].supersedes = Some(previous_ref);
    successor_plan.plan_digest = successor_plan.computed_digest()?;
    let successor = crate::intent_admission::build_successor_admission(
        successor_plan,
        crate::IntentAcceptanceKind::ExplicitUserConfirmation,
        replacement_turn_id,
        "authority-continuity-v2".to_owned(),
    )?;
    crate::append_successor_intent_plan_admission(&mut session, &successor, None)?;

    let first_active =
        CompactionSidecarProjection::from_records(&store.read_event_records_writer()?)?
            .latest_for_branch(None)
            .expect("first compaction is active")
            .clone();
    execute_with_target(
        &store,
        request(
            &store,
            "anchor-attempt-2",
            "anchor-compaction-2",
            Some(first_active.folded_through),
        )?,
        |checkpoint, task_memory, candidate| {
            target_material(&session_scope_id, checkpoint, task_memory, candidate)
        },
    )?;

    session.append_user_message(ModelMessage::user("Continue after the second compaction."))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("Continuing with the accepted boundary.".to_owned()),
        Vec::new(),
    ))?;
    let second_active =
        CompactionSidecarProjection::from_records(&store.read_event_records_writer()?)?
            .latest_for_branch(None)
            .expect("second compaction is active")
            .clone();
    execute_with_target(
        &store,
        request(
            &store,
            "anchor-attempt-3",
            "anchor-compaction-3",
            Some(second_active.folded_through),
        )?,
        |checkpoint, task_memory, candidate| {
            target_material(&session_scope_id, checkpoint, task_memory, candidate)
        },
    )?;

    let records = store.read_event_records_writer()?;
    let sidecars = CompactionSidecarProjection::from_records(&records)?;
    let active = sidecars
        .latest_for_branch(None)
        .expect("third compaction is active");
    let anchor = active
        .checkpoint
        .session_anchor
        .as_ref()
        .expect("accepted session has an authority anchor");
    anchor.validate_against_records(&records)?;
    assert_eq!(
        anchor.root_objective.exact_text,
        "Implement portable continuity without losing accepted authority."
    );
    let active_constraints = anchor
        .constraints
        .iter()
        .filter(|constraint| constraint.status == crate::ConstraintStatusV1::Active)
        .collect::<Vec<_>>();
    assert_eq!(active_constraints.len(), 1);
    assert_eq!(
        active_constraints[0].exact_text,
        "Commits are allowed, but do not push."
    );
    assert_eq!(active_constraints[0].supersedes.len(), 1);
    assert!(anchor.constraints.iter().any(|constraint| {
        constraint.exact_text == "Do not commit or push."
            && constraint.status == crate::ConstraintStatusV1::Superseded
    }));
    let rendered = active
        .checkpoint
        .render_for_provider(&active.task_memory)?
        .content
        .expect("checkpoint content");
    assert!(rendered.contains("Commits are allowed, but do not push."));
    assert!(!rendered.contains("Do not commit or push."));
    assert!(!rendered.contains("Continue after the second compaction."));
    Ok(())
}

#[test]
fn accepted_anchor_keeps_exact_active_spans_without_pinning_a_large_first_turn() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("large-first-turn.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    let corpus = "large-corpus-line\n".repeat(12_000);
    let first = ModelMessage::user(format!(
        "Implement source-bound continuity.\n\nReference corpus:\n{corpus}"
    ));
    let source_turn_id = first.id.clone();
    session.append_user_message(first)?;
    session.append_assistant_message(ModelMessage::assistant(
        Some(
            "I will preserve only accepted active spans and a durable corpus reference.".to_owned(),
        ),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user("Continue with the implementation."))?;

    let admission = crate::admit_user_declared_root(
        &crate::IntentAdmissionContextV1::initial(
            crate::IntentStackId::new("stack-large-first-turn")?,
            crate::stable_workspace_id(temp.path())?,
            session.session_scope_id(),
        )?,
        crate::UserDeclaredIntentV1 {
            title: "Implement source-bound continuity".to_owned(),
            statement: "Implement source-bound continuity.".to_owned(),
            acceptance_criteria: vec![crate::IntentProposalCriterionV1 {
                criterion_alias: "delivery".to_owned(),
                statement: "Do not push.".to_owned(),
                required: true,
            }],
        },
        &crate::IntentAcceptanceAuthorityV1::user_declared_root(
            source_turn_id,
            "authority-large-first-turn",
        )?,
    )?;
    crate::append_chat_root_intent_admission(&session, &admission)?;

    let session_scope_id = session_scope_id(&store)?;
    execute_with_target(
        &store,
        request(
            &store,
            "large-first-attempt",
            "large-first-compaction",
            None,
        )?,
        |checkpoint, task_memory, candidate| {
            target_material(&session_scope_id, checkpoint, task_memory, candidate)
        },
    )?;

    let records = store.read_event_records_writer()?;
    let active = CompactionSidecarProjection::from_records(&records)?
        .latest_for_branch(None)
        .expect("large first-turn compaction is active")
        .clone();
    let anchor = active
        .checkpoint
        .session_anchor
        .as_ref()
        .expect("accepted Intent produces an anchor");
    assert_eq!(
        anchor.root_objective.exact_text,
        "Implement source-bound continuity."
    );
    let rendered = active
        .checkpoint
        .render_for_provider(&active.task_memory)?
        .content
        .expect("checkpoint content");
    assert!(rendered.contains("Implement source-bound continuity."));
    assert!(rendered.contains("Do not push."));
    assert!(!rendered.contains("large-corpus-line"));
    assert!(rendered.len() < 16 * 1024);
    Ok(())
}

#[test]
fn legacy_anchor_bounds_a_large_first_turn_and_keeps_a_durable_body_reference() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("legacy-large-first-turn.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    let corpus = "legacy-large-corpus-line\n".repeat(12_000);
    session.append_user_message(ModelMessage::user(format!(
        "Implement the bounded legacy fallback.\n\nReference corpus:\n{corpus}"
    )))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("I will retain a durable transcript reference.".to_owned()),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user("Continue."))?;

    let session_scope_id = session_scope_id(&store)?;
    execute_with_target(
        &store,
        request(
            &store,
            "legacy-large-first-attempt",
            "legacy-large-first-compaction",
            None,
        )?,
        |checkpoint, task_memory, candidate| {
            target_material(&session_scope_id, checkpoint, task_memory, candidate)
        },
    )?;

    let records = store.read_event_records_writer()?;
    let sidecars = CompactionSidecarProjection::from_records(&records)?;
    let active = sidecars
        .latest_for_branch(None)
        .expect("legacy large first-turn compaction is active");
    let anchor = active
        .checkpoint
        .session_anchor
        .as_ref()
        .expect("legacy session has an anchor");
    assert_eq!(
        anchor.root_objective.exact_text,
        "Implement the bounded legacy fallback."
    );
    let body = anchor
        .attachment_refs
        .iter()
        .find(|artifact| artifact.media_type == "text/plain; charset=utf-8")
        .expect("oversized first turn has a durable transcript reference");
    assert!(body.byte_size > 200_000);
    assert!(body.retrieval_ref.contains("session-event:"));
    let rendered = active
        .checkpoint
        .render_for_provider(&active.task_memory)?
        .content
        .expect("checkpoint content");
    assert!(!rendered.contains("legacy-large-corpus-line"));
    Ok(())
}

#[test]
fn continuity_anchor_does_not_reactivate_terminal_task_permissions() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("terminal-permission.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    session.append_user_message(ModelMessage::user("Implement the scoped task."))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("The task will use a scoped permission.".to_owned()),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user("Continue."))?;
    let plan_id = crate::PlanId::new("terminal-permission-plan")?;
    let task_id = crate::TaskId::new("terminal-permission-task")?;
    let plan_hash = crate::plan_text_hash("terminal permission plan");
    session.append_control(crate::ControlEntry::TaskCreatedFromPlan(
        crate::TaskCreatedFromPlanEntry {
            plan_id: plan_id.clone(),
            plan_hash: plan_hash.clone(),
            task_id: task_id.clone(),
            task_plan_version: 1,
            step_mapping: Vec::new(),
            stale_reason: None,
            created_at_ms: 5,
        },
    ))?;
    session.append_control(crate::ControlEntry::PlanPermissionGranted(
        crate::PlanPermissionGrantedEntry {
            plan_id,
            plan_hash,
            task_id: task_id.clone(),
            workspace_snapshot_id: Some("snapshot-v1".to_owned()),
            permission: crate::PlanApprovalPermission::WorkspaceEdits,
            scope: crate::PlanApprovalScope {
                summary: "edit the scoped file".to_owned(),
                workspace_paths: vec!["src/lib.rs".to_owned()],
            },
            expires: crate::PlanApprovalExpiry::Session,
            granted_at_ms: 6,
        },
    ))?;
    session.append_control(crate::ControlEntry::TaskRun(crate::TaskRunEntry {
        task_id,
        parent_session_ref: crate::SessionRef::new_relative("parent.jsonl")?,
        objective: "Implement the scoped task".to_owned(),
        status: crate::TaskRunStatus::Completed,
        reason: Some("done".to_owned()),
    }))?;

    let session_scope_id = session_scope_id(&store)?;
    execute_with_target(
        &store,
        request(
            &store,
            "terminal-permission-attempt",
            "terminal-permission-compaction",
            None,
        )?,
        |checkpoint, task_memory, candidate| {
            target_material(&session_scope_id, checkpoint, task_memory, candidate)
        },
    )?;
    let records = store.read_event_records_writer()?;
    let sidecars = CompactionSidecarProjection::from_records(&records)?;
    let active = sidecars
        .latest_for_branch(None)
        .expect("terminal permission compaction is active");
    let anchor = active
        .checkpoint
        .session_anchor
        .as_ref()
        .expect("continuity anchor exists");
    assert!(anchor.authorization_boundary.is_empty());
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
