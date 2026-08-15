use super::*;
use crate::app::modal_flow::ModelCatalogState;
use crate::app::tests::common::adaptive_test_compaction_preview;
use crate::runner::{
    TerminalTaskControlIdentity, ToolOutputShrinkPreview, V2CompactionAdmission,
    V2CompactionPreviewState, V2CompactionReview, V2ContinuityPreview,
};
use crate::{app::MutationArtifactRetentionPreview, approval::PendingApproval};

fn connection_catalog_result(
    app: &AppState,
    state: sigil_runtime::provider_connections::ModelCatalogState,
    models: &[&str],
) -> sigil_runtime::provider_connections::ModelCatalogResult {
    let pending = app
        .runtime
        .active_model_picker_refresh
        .as_ref()
        .expect("connection refresh should be pending");
    let connection_id = pending
        .connection_id
        .clone()
        .expect("connection refresh should carry an id");
    sigil_runtime::provider_connections::ModelCatalogResult {
        request_id: pending.request_id,
        connection_id: connection_id.clone(),
        draft_revision: pending.draft_revision,
        connection_fingerprint: pending
            .connection_fingerprint
            .clone()
            .expect("connection refresh should carry a fingerprint"),
        state,
        entries: models
            .iter()
            .map(
                |model| sigil_runtime::provider_connections::ModelCatalogEntry {
                    model_ref: sigil_kernel::ModelRef::new(
                        connection_id.clone(),
                        (*model).to_owned(),
                    )
                    .expect("test model ref"),
                    display_name: (*model).to_owned(),
                    availability: sigil_runtime::provider_connections::ModelAvailability::Available,
                    recommendation:
                        sigil_runtime::provider_connections::ModelRecommendation::Standard,
                    provenance: sigil_runtime::provider_connections::ModelCatalogProvenance::Remote,
                },
            )
            .collect(),
        retry_after_secs: None,
        manual_entry_allowed: false,
    }
}

fn structured_plan_text(summary: &str, title: &str, path: &str) -> String {
    format!(
        r#"Plan:

```sigil-plan-v2
{{
  "summary": "{summary}",
  "steps": [
    {{
      "step_id": "step-1",
      "title": "{title}",
      "target_paths": ["{path}"]
    }}
  ],
  "target_paths": ["{path}"]
}}
```
"#
    )
}

#[test]
fn normal_input_creates_user_and_running_state() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "hello".to_owned();
    let action = app.submit_input()?;
    assert!(
        app.timeline
            .iter()
            .any(|entry| { entry.role == TimelineRole::User && entry.text == "hello" })
    );
    assert!(matches!(action, Some(AppAction::SubmitPrompt(prompt)) if prompt == "hello"));
    assert!(app.runtime.is_busy);
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert_eq!(app.composer_height(), 5);
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Phase)
    );
    assert!(
        app.events.iter().any(|event| {
            event.label == "phase" && event.detail == "thinking|deepseek-v4-flash"
        })
    );
    assert_eq!(app.run_phase(), RunPhase::Thinking);
    assert_eq!(app.last_notice(), Some("thinking"));
    Ok(())
}

#[test]
fn sensitive_prompt_stays_exact_in_action_and_live_history_but_safe_on_tui_surfaces() -> Result<()>
{
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let raw = "inspect https://example.com/private?signature=tui-secret exactly";
    app.composer.input = raw.to_owned();

    let action = app.submit_input()?;

    assert!(matches!(action, Some(AppAction::SubmitPrompt(prompt)) if prompt == raw));
    assert_eq!(app.composer.input_history, vec![raw.to_owned()]);
    assert!(
        app.timeline
            .iter()
            .all(|entry| !entry.text.contains("tui-secret"))
    );
    assert!(
        app.events
            .iter()
            .all(|event| !event.detail.contains("tui-secret"))
    );

    app.handle_worker_message(WorkerMessage::ConversationQueueDispatchStarted {
        queue_id: sigil_kernel::ConversationInputQueueId::new("queue_1")?,
        prompt: raw.to_owned(),
    })?;
    app.handle_worker_message(WorkerMessage::TaskRunStarted {
        task_id: "task_1".to_owned(),
        objective: raw.to_owned(),
    })?;

    assert!(
        app.timeline
            .iter()
            .all(|entry| !entry.text.contains("tui-secret"))
    );
    assert!(
        app.events
            .iter()
            .all(|event| !event.detail.contains("tui-secret"))
    );
    Ok(())
}

#[test]
fn task_provider_route_diagnostics_are_live_only_and_clear_at_task_boundary() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_worker_message(WorkerMessage::TaskRunStarted {
        task_id: "task_1".to_owned(),
        objective: "inspect routes".to_owned(),
    })?;
    app.handle_worker_message(WorkerMessage::TaskProviderRouteDiagnosticsUpdated {
        snapshot: sigil_runtime::TaskProviderRouteDiagnosticsSnapshot {
            routes: vec![sigil_runtime::TaskProviderRouteDiagnostics {
                route_fingerprint: "sha256:route-1".to_owned(),
                provider_name: "deepseek".to_owned(),
                model_name: "deepseek-v4-flash".to_owned(),
                consumers: vec![sigil_runtime::TaskProviderRouteConsumerDiagnostics {
                    consumer: sigil_runtime::TaskProviderRouteConsumer::Planner,
                    in_flight: 1,
                    waiting: 0,
                }],
                in_flight: 1,
                waiting: 0,
                concurrency_window: 2,
                max_concurrency: 4,
                cooldown_remaining_ms: 0,
                consecutive_rate_limits: 1,
            }],
        },
    })?;
    app.handle_worker_message(WorkerMessage::TaskCompletionProgressUpdated {
        snapshot: sigil_runtime::TaskCompletionProgressSnapshot {
            batch: Some(sigil_runtime::TaskCompletionProgress {
                generation: 1,
                task_id: "task_1".to_owned(),
                plan_version: 1,
                arrived: 1,
                total: 2,
                members: vec![
                    sigil_runtime::TaskCompletionProgressMember {
                        step_id: "read_a".to_owned(),
                        title: "Read A".to_owned(),
                        request_order: 1,
                        arrival_order: None,
                        outcome: None,
                    },
                    sigil_runtime::TaskCompletionProgressMember {
                        step_id: "read_b".to_owned(),
                        title: "Read B".to_owned(),
                        request_order: 2,
                        arrival_order: Some(1),
                        outcome: Some(sigil_runtime::TaskCompletionOutcome::Succeeded),
                    },
                ],
            }),
        },
    })?;

    assert_eq!(app.runtime.task_provider_route_diagnostics.routes.len(), 1);
    assert_eq!(
        app.runtime
            .task_completion_progress
            .batch
            .as_ref()
            .map(|batch| batch.arrived),
        Some(1)
    );
    let strip = app
        .task_strip_view()
        .expect("live task should render before durable projection arrives");
    assert_eq!(strip.title, "Task task_1");
    assert_eq!(strip.detail, "running · awaiting durable projection");
    assert_eq!(strip.rows[0].label, "inspect routes");
    assert!(
        app.task_sidebar_lines()
            .iter()
            .any(|line| line.contains("planner → deepseek/deepseek-v4-flash"))
    );
    assert!(
        app.task_sidebar_lines()
            .iter()
            .any(|line| line.contains("arrival #1 → commit #2"))
    );

    app.handle_worker_message(WorkerMessage::TaskRunStarted {
        task_id: "task_2".to_owned(),
        objective: "start clean".to_owned(),
    })?;
    assert!(
        app.runtime
            .task_provider_route_diagnostics
            .routes
            .is_empty(),
        "a new task must not inherit stale live route attribution"
    );
    assert!(
        app.runtime.task_completion_progress.batch.is_none(),
        "a new task must not inherit stale live completion order"
    );
    Ok(())
}

#[test]
fn run_notice_filters_status_noise_but_keeps_errors() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::Notice("agent agent_chat_1 finished".to_owned()))?;
    assert_eq!(app.last_notice(), Some("agent agent_chat_1 finished"));
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice)
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "notice" && event.detail == "agent agent_chat_1 finished")
    );

    app.handle(RunEvent::Notice(
        "permission wait_agent subject=agent:agent_chat_1 mode=allow".to_owned(),
    ))?;
    assert_eq!(
        app.last_notice(),
        Some("permission wait_agent subject=agent:agent_chat_1 mode=allow")
    );
    assert!(!app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice && entry.text.contains("permission wait_agent")
    }));

    app.handle(RunEvent::Notice(
        "agent budget warning after child completion: max exceeded".to_owned(),
    ))?;
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice
            && entry
                .text
                .contains("agent budget warning after child completion")
    }));
    Ok(())
}

#[test]
fn activate_lazy_mcp_action_maps_to_worker_command() {
    let app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    let command = app.into_worker_command(AppAction::ActivateLazyMcp {
        server_name: Some("filesystem".to_owned()),
    });

    assert!(matches!(
        command,
        WorkerCommand::ActivateLazyMcp {
            server_name: Some(ref server_name)
        } if server_name == "filesystem"
    ));

    let command = app.into_worker_command(AppAction::RefreshMcpServer {
        server_name: "filesystem".to_owned(),
    });
    assert!(matches!(
        command,
        WorkerCommand::RefreshMcpServer { ref server_name } if server_name == "filesystem"
    ));
}

#[test]
fn plan_actions_map_to_worker_commands() {
    let app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    let submit = app.into_worker_command(AppAction::SubmitTask("ship task".to_owned()));
    assert!(matches!(
        submit,
        WorkerCommand::SubmitTask { ref prompt } if prompt == "ship task"
    ));

    let plan_prompt = app.into_worker_command(AppAction::SubmitPlanPrompt(
        "inspect before editing".to_owned(),
    ));
    assert!(matches!(
        plan_prompt,
        WorkerCommand::SubmitPlanPrompt { ref prompt, .. }
            if prompt == "inspect before editing"
    ));

    let continue_task = app.into_worker_command(AppAction::ContinueTask {
        task_id: Some("task_1".to_owned()),
        guidance: Some("focus runtime".to_owned()),
    });
    assert!(matches!(
        continue_task,
        WorkerCommand::ContinueTask {
            task_id: Some(ref task_id),
            guidance: Some(ref guidance)
        } if task_id == "task_1" && guidance == "focus runtime"
    ));

    assert!(matches!(
        app.into_worker_command(AppAction::CleanMutationArtifacts {
            target: sigil_kernel::MutationArtifactCleanupTarget::Recommended,
        }),
        WorkerCommand::CleanMutationArtifacts {
            target: sigil_kernel::MutationArtifactCleanupTarget::Recommended,
        }
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::DeleteMutationArtifact {
            artifact_id: "mutation-artifact:sha256:abc".to_owned(),
        }),
        WorkerCommand::DeleteMutationArtifact { ref artifact_id }
            if artifact_id == "mutation-artifact:sha256:abc"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::ApproveVerificationCheck {
            check_spec_id: "cargo-test".to_owned(),
        }),
        WorkerCommand::ApproveVerificationCheck { ref check_spec_id }
            if check_spec_id == "cargo-test"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::SandboxVerificationCheck {
            check_spec_id: "cargo-test".to_owned(),
        }),
        WorkerCommand::SandboxVerificationCheck { ref check_spec_id }
            if check_spec_id == "cargo-test"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::RerunTaskVerification {
            request: sigil_kernel::TaskVerificationRerunRequest::new(
                sigil_kernel::TaskId::new("task_1").expect("task id"),
                1,
                sigil_kernel::TaskStepId::new("step_1").expect("step id"),
                "cargo-test".to_owned(),
                "check-hash".to_owned(),
                "policy-hash".to_owned(),
                "snapshot-1".to_owned(),
            ),
        }),
        WorkerCommand::RerunTaskVerification { ref request }
            if request.check_spec_id == "cargo-test"
                && request.workspace_snapshot_id == "snapshot-1"
    ));
    let integration_request = sigil_kernel::TaskIntegrationReviewRequest {
        request_id: "integration-review-request".to_owned(),
        task_id: sigil_kernel::TaskId::new("task_1").expect("task id"),
        plan_id: sigil_kernel::IntegrationPlanId::new("plan-1").expect("plan id"),
        plan_version: 1,
        preview_digest: format!("sha256:{}", "a".repeat(64)),
    };
    assert!(matches!(
        app.into_worker_command(AppAction::ReviewTaskIntegration {
            request: integration_request.clone(),
        }),
        WorkerCommand::ReviewTaskIntegration { request }
            if request == integration_request
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::AcceptTaskIntegration {
            request: integration_request.clone(),
        }),
        WorkerCommand::AcceptTaskIntegration { request }
            if request == integration_request
    ));
}

#[test]
fn intent_stack_actions_map_to_bounded_worker_commands() {
    let app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let intent_ref = sigil_kernel::IntentVersionRef::new(
        sigil_kernel::IntentId::new("intent_leaf").expect("intent id"),
        1,
    )
    .expect("intent ref");
    let request = sigil_kernel::IntentDropRequestV1 {
        operation_id: sigil_kernel::IntentOperationId::new("operation_drop_leaf")
            .expect("operation id"),
        stack_version: sigil_kernel::IntentStackVersion::new(7).expect("stack version"),
        preview_digest: sigil_kernel::IntentDigest::new(format!(
            "sha256:jcs-v1:{}",
            "a".repeat(64)
        ))
        .expect("preview digest"),
    };

    assert!(matches!(
        app.into_worker_command(AppAction::LoadIntentStack { request_id: 10 }),
        WorkerCommand::LoadIntentStack { request_id: 10 }
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::PreviewIntentDrop {
            request_id: 11,
            intent_ref: intent_ref.clone(),
        }),
        WorkerCommand::PreviewIntentDrop {
            request_id: 11,
            intent_ref: converted,
        } if converted == intent_ref
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::ExecuteIntentDrop {
            request_id: 12,
            request: request.clone(),
        }),
        WorkerCommand::ExecuteIntentDrop {
            request_id: 12,
            request: converted,
        } if converted == request
    ));
}

#[test]
fn plan_run_finished_surfaces_pending_plan_approval_and_key_actions() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let base_snapshot = app
        .config_snapshot
        .as_ref()
        .and_then(|root_config| {
            sigil_runtime::plan_handoff_workspace_snapshot_id(root_config, &app.workspace_root)
                .ok()
                .flatten()
        })
        .expect("test workspace snapshot");
    let mut review_session = sigil_kernel::Session::new("planned", "planned-model");
    let review_request = sigil_runtime::PlanReviewCoordinator::prepare_explicit_plan_review(
        &mut review_session,
        "Inspect and edit README.md",
        "worker-bridge-plan-review",
        Some(base_snapshot.clone()),
        1,
    )?;
    let draft = sigil_kernel::plan_draft_created_entry_with_plan_id(
        review_request.plan_id.clone(),
        &structured_plan_text(
            "Inspect and edit README.md",
            "Apply the approved copy edit",
            "README.md",
        ),
        review_request.plan_source_ref(),
        2,
        Some(base_snapshot),
    )?
    .expect("non-empty plan should create draft");
    sigil_runtime::PlanReviewCoordinator::commit_draft_from_child(
        &mut review_session,
        &draft,
        &review_request,
        3,
    )?;

    app.handle_worker_message(WorkerMessage::PlanRunFinished {
        result: sigil_kernel::AgentRunResult {
            final_text: draft.inline_text.clone().unwrap_or_default(),
            tool_calls: 0,
            final_message_id: None,
        },
        entries: review_session.entries().to_vec(),
    })?;

    let pending = app
        .pending_plan_approval()
        .expect("plan output should create a pending approval");
    assert_eq!(pending.plan_id.as_deref(), Some(draft.plan_id.as_str()));
    assert!(pending.plan_hash.starts_with("sha256:"));
    assert_eq!(pending.summary, "Inspect and edit README.md");
    assert_eq!(pending.target_path_count, 1);
    assert_eq!(pending.suggested_check_count, 0);
    assert_eq!(pending.steps, vec!["Apply the approved copy edit"]);
    assert_eq!(app.composer_mode_label(), "Plan");
    assert_eq!(app.last_notice(), Some("plan ready"));

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(
        action.is_none(),
        "first Enter opens the complete plan review"
    );
    assert!(
        app.pending_plan_approval()
            .is_some_and(|plan| plan.workbench_open)
    );
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(app.pending_plan_approval().is_none());
    assert!(matches!(
        action,
        Some(AppAction::CreateTaskFromPlan {
            plan_id,
            expected_plan_hash,
            start_mode: sigil_kernel::PlanTaskStartMode::CreateAndRun,
            permission_grant: None,
        }) if plan_id == draft.plan_id.as_str() && expected_plan_hash == draft.plan_hash
    ));

    app.handle_worker_message(WorkerMessage::PlanRunFinished {
        result: sigil_kernel::AgentRunResult {
            final_text: "   ".to_owned(),
            tool_calls: 1,
            final_message_id: None,
        },
        entries: Vec::new(),
    })?;
    assert!(app.pending_plan_approval().is_none());
    assert_eq!(app.last_notice(), Some("plan finished"));
    Ok(())
}

#[test]
fn plan_ready_bare_letters_stay_composer_input() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let draft = sigil_kernel::plan_draft_created_entry(
        &structured_plan_text("Update README", "Update README.md", "README.md"),
        sigil_kernel::PlanSourceRef::default(),
        1,
        Some("snapshot-1".to_owned()),
    )?
    .expect("non-empty plan should create draft");
    app.set_pending_plan_approval_from_draft(&draft, None);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(app.pending_plan_approval().is_some());
    assert_eq!(app.composer.input, "x");
    assert_eq!(app.last_notice(), None);
    Ok(())
}

#[test]
fn pending_plan_approval_non_empty_input_submits_normally() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_worker_message(WorkerMessage::PlanRunFinished {
        result: sigil_kernel::AgentRunResult {
            final_text: "1. inspect\n2. revise plan".to_owned(),
            tool_calls: 0,
            final_message_id: None,
        },
        entries: Vec::new(),
    })?;
    assert!(app.pending_plan_approval().is_none());
    app.composer.input = "/plan revise this plan".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(app.pending_plan_approval().is_none());
    assert!(matches!(
        action,
        Some(AppAction::SubmitPlanPrompt(prompt)) if prompt == "revise this plan"
    ));
    Ok(())
}

#[test]
fn pending_durable_plan_explicit_reject_requests_worker_rejection() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let draft = sigil_kernel::plan_draft_created_entry(
        &structured_plan_text("Update README", "Update README.md", "README.md"),
        sigil_kernel::PlanSourceRef::default(),
        1,
        Some("snapshot-1".to_owned()),
    )?
    .expect("non-empty plan should create draft");
    app.set_pending_plan_approval_from_draft(&draft, None);

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
            .is_none()
    );
    for _ in 0..3 {
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    }
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(app.pending_plan_approval().is_some());
    assert!(matches!(
        action,
        Some(AppAction::RejectPlan {
            plan_id,
            expected_plan_hash,
        }) if plan_id == draft.plan_id.as_str() && expected_plan_hash == draft.plan_hash
    ));
    assert_eq!(app.last_notice(), Some("rejecting plan"));
    assert!(
        app.events
            .iter()
            .any(|event| { event.label == "plan" && event.detail == "reject" })
    );
    Ok(())
}

#[test]
fn unstructured_plan_finished_does_not_create_pending_surface() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_worker_message(WorkerMessage::PlanRunFinished {
        result: sigil_kernel::AgentRunResult {
            final_text: "1. inspect\n2. revise plan".to_owned(),
            tool_calls: 0,
            final_message_id: None,
        },
        entries: Vec::new(),
    })?;
    assert!(app.pending_plan_approval().is_none());

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(app.pending_plan_approval().is_none());
    assert_eq!(app.composer_mode_label(), "Build");
    assert_eq!(app.last_notice(), Some("plan finished"));
    Ok(())
}

#[test]
fn plan_rejected_message_syncs_session_and_clears_pending_surface() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let draft = sigil_kernel::plan_draft_created_entry(
        &structured_plan_text("Update README", "Update README.md", "README.md"),
        sigil_kernel::PlanSourceRef::default(),
        1,
        Some("snapshot-1".to_owned()),
    )?
    .expect("non-empty plan should create draft");
    app.set_pending_plan_approval_from_draft(&draft, None);
    let entry = sigil_kernel::PlanDecisionRecordedEntry {
        plan_id: draft.plan_id.clone(),
        plan_hash: draft.plan_hash.clone(),
        decision: sigil_kernel::PlanDecision::Rejected,
        decided_by: sigil_kernel::PlanDecisionActor::User,
        decided_at_ms: 42,
        reason: Some("discarded plan".to_owned()),
    };

    app.handle_worker_message(WorkerMessage::PlanRejected {
        entry: entry.clone(),
        entries: vec![
            SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft)),
            SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(entry.clone())),
        ],
    })?;

    assert!(app.pending_plan_approval().is_none());
    let expected_notice = format!("plan {} rejected", entry.plan_id.as_str());
    assert_eq!(app.last_notice(), Some(expected_notice.as_str()));
    let projection =
        sigil_kernel::PlanArtifactProjection::from_entries(&app.session_browser.current_entries);
    assert!(projection.latest_pending_plan().is_none());
    assert_eq!(projection.latest_decision(&entry.plan_id), Some(&entry));
    Ok(())
}

#[test]
fn run_failed_surfaces_root_cause_summary_in_notice() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.review.checkpoint_action_pending = true;

    app.handle_worker_message(WorkerMessage::RunFailed(
        "deepseek request failed\n\nCaused by:\n    0: failed to send DeepSeek request\n    1: error sending request for url (https://api.example.com)"
            .to_owned(),
    ))?;

    assert_eq!(
        app.last_notice(),
        Some("error sending request for url (https://api.example.com)")
    );
    assert!(!app.review.checkpoint_action_pending);
    assert!(app.timeline.iter().any(|entry| {
        entry
            .text
            .contains("error sending request for url (https://api.example.com)")
    }));
    assert!(app.events.iter().any(
        |event| event.label == "run:error" && event.detail.contains("deepseek request failed")
    ));
    Ok(())
}

#[test]
fn empty_v2_compaction_preview_keeps_usage_status_and_reports_no_foldable_history() -> Result<()> {
    let mut config = test_config();
    config.agent.runtime_provider = "planned".to_owned();
    config.agent.model = "planned-model".to_owned();
    config.compaction.context_window_tokens = Some(100);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 90,
        completion_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 90,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
        cache_usage: None,
        pricing_snapshot: None,
    }))?;
    assert_eq!(app.runtime.compaction_status, "soft");

    app.handle_worker_message(WorkerMessage::V2CompactionPreviewed {
        state: V2CompactionPreviewState::NoFoldableHistory {
            durable_message_count: 4,
            minimum_tail_turn_count: 6,
        },
    })?;

    assert_eq!(app.runtime.compaction_status, "soft");
    assert_eq!(app.runtime.stats.last_prompt_tokens, 90);
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice
            && entry.text == "no newly foldable history: 4 durable message(s); at least 6 complete turns remain live. Add completed turns before compacting again."
    }));
    Ok(())
}

#[test]
fn ctrl_c_then_run_cancelled_restores_durable_session_view() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        config_version: 2,
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = resolved_session_log_dir(&config, temp.path());
    std::fs::create_dir_all(&session_dir)?;
    let restored_path = session_dir.join("session-cancelled.jsonl");
    let restored = restored_entries("cancel-provider", "cancel-model");
    write_session_log(&restored_path, &restored)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.composer.input = "volatile prompt".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "volatile prompt"
    ));
    assert!(app.runtime.is_busy);

    let cancel_action =
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;
    assert!(matches!(cancel_action, Some(AppAction::CancelRun)));
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("cancel requested"))
    );

    let entries = JsonlSessionStore::read_entries(&restored_path)?;
    app.handle_worker_message(WorkerMessage::RunCancelled {
        session_log_path: restored_path.clone(),
        provider_name: "cancel-provider".to_owned(),
        model_name: "cancel-model".to_owned(),
        entries,
    })?;

    assert!(!app.runtime.is_busy);
    assert!(app.approval.pending.is_none());
    assert_eq!(app.runtime.provider_name, "cancel-provider");
    assert_eq!(app.runtime.model_name, "cancel-model");
    assert_eq!(app.session_id, "cancelled");
    assert_eq!(app.session_log_path, restored_path);
    assert!(
        app.timeline
            .iter()
            .any(|entry| { entry.text.contains("run cancelled; restored") })
    );
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.text == "volatile prompt")
    );
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text == "restored assistant answer")
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "restore" && event.detail == "entries=4")
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "model" && event.detail == "cancel-provider/cancel-model")
    );
    Ok(())
}

#[test]
fn task_pause_messages_restore_the_resumable_durable_session_view() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = resolved_session_log_dir(&config, temp.path());
    std::fs::create_dir_all(&session_dir)?;
    let restored_path = session_dir.join("session-task-paused.jsonl");
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let mut entries = restored_entries("pause-provider", "pause-model");
    entries.push(SessionLogEntry::Control(ControlEntry::TaskRun(
        sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "pause safely".to_owned(),
            title: None,
            status: sigil_kernel::TaskRunStatus::Paused,
            reason: Some("task paused from TUI".to_owned()),
        },
    )));
    write_session_log(&restored_path, &entries)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.composer.input = "start a durable task".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(_))
    ));
    app.handle_worker_message(WorkerMessage::TaskPauseRequested {
        task_id: task_id.as_str().to_owned(),
    })?;
    assert_eq!(
        app.last_notice(),
        Some("pausing task task_1 — waiting for active work to stop")
    );
    assert!(app.events.iter().any(|event| {
        event.label == "task:pause" && event.detail == "task task_1 pause requested"
    }));

    app.handle_worker_message(WorkerMessage::TaskRunPaused {
        task_id: task_id.as_str().to_owned(),
        session_log_path: restored_path.clone(),
        provider_name: "pause-provider".to_owned(),
        model_name: "pause-model".to_owned(),
        entries,
    })?;

    assert!(!app.runtime.is_busy);
    assert_eq!(app.run_phase(), RunPhase::Idle);
    assert_eq!(app.runtime.provider_name, "pause-provider");
    assert_eq!(app.runtime.model_name, "pause-model");
    assert_eq!(app.session_id, "task-paused");
    assert_eq!(app.session_log_path, restored_path);
    assert_eq!(
        app.last_notice(),
        Some("task task_1 paused; use /task continue to resume")
    );
    Ok(())
}

#[test]
fn esc_interrupts_active_run() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "long task".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "long task"
    ));
    assert!(app.runtime.is_busy);

    let cancel_action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(matches!(cancel_action, Some(AppAction::CancelRun)));
    assert_eq!(app.last_notice(), Some("cancellation requested"));
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("cancel requested"))
    );
    Ok(())
}

#[test]
fn worker_messages_apply_balance_and_model_refresh() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_model_picker(ModelPickerTarget::Provider, "custom-model");
    let model_request = app
        .runtime
        .active_model_picker_refresh
        .as_ref()
        .expect("model picker refresh should be active");
    let model_connection_id = model_request
        .connection_id
        .clone()
        .expect("exact connection id");
    let catalog = connection_catalog_result(
        &app,
        sigil_runtime::provider_connections::ModelCatalogState::Remote,
        &["remote-model"],
    );

    let balance_request_id = app.next_background_request_id();
    app.runtime.active_balance_refresh_id = Some(balance_request_id);
    app.handle_worker_message(WorkerMessage::ProviderBalanceRefreshed {
        request_id: balance_request_id,
        snapshot: sigil_runtime::BalanceSnapshot {
            total: Some(2.0),
            currency: Some("USD".to_owned()),
            available: true,
            status: "USD 2.00".to_owned(),
        },
    })?;
    app.handle_worker_message(WorkerMessage::ConnectionModelsRefreshed { result: catalog })?;

    assert_eq!(app.runtime.balance_snapshot.status, "USD 2.00");
    assert!(app.runtime.active_balance_refresh_id.is_none());
    assert!(app.runtime.active_model_picker_refresh.is_none());
    let expected_notice = format!("model catalog remote for {model_connection_id}");
    assert_eq!(app.last_notice(), Some(expected_notice.as_str()));
    assert!(app.modal_lines().join("\n").contains("remote-model"));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "balance" && event.detail == "USD 2.00")
    );
    Ok(())
}

#[test]
fn model_picker_worker_empty_refresh_enters_empty_state_without_candidates() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_model_picker(ModelPickerTarget::Provider, "custom-model");
    let model_request = app
        .runtime
        .active_model_picker_refresh
        .as_ref()
        .expect("model picker refresh should be active");
    let _request_id = model_request.request_id;
    let catalog = connection_catalog_result(
        &app,
        sigil_runtime::provider_connections::ModelCatalogState::Empty,
        &[],
    );

    app.handle_worker_message(WorkerMessage::ConnectionModelsRefreshed { result: catalog })?;

    let lines = app.modal_lines().join("\n");
    assert!(lines.contains("provider returned no models"));
    assert!(lines.contains("configured: custom-model"));
    if let Some(ModalState::ModelPicker(state)) = app.modal_state.as_ref() {
        assert!(state.options.is_empty());
        assert_eq!(state.catalog_state, ModelCatalogState::Empty);
    } else {
        panic!("expected model picker modal");
    }
    Ok(())
}

#[test]
fn provider_model_worker_command_debug_redacts_credentials() {
    let command = WorkerCommand::RefreshProviderModels {
        request_id: 7,
        provider_config: sigil_runtime::ProviderStatusConfig {
            api_key: Some("worker-model-list-secret".to_owned()),
            base_url: "https://models.example/v1".to_owned(),
            request_timeout_secs: 5,
        },
    };

    let debug = format!("{command:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("worker-model-list-secret"));
}

#[test]
fn model_picker_pending_worker_commands_and_stale_provider_refreshes_are_noops() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let _ = app.drain_pending_worker_commands();
    assert!(!app.poll_background_tasks());
    assert!(!app.has_pending_worker_commands());

    app.open_model_picker(ModelPickerTarget::Provider, "custom-model");
    let model_request = app
        .runtime
        .active_model_picker_refresh
        .as_ref()
        .expect("model picker refresh should be active");
    let model_request_id = model_request.request_id;
    let model_base_url = model_request.base_url.clone();
    assert!(app.has_pending_worker_commands());

    let commands = app.drain_pending_worker_commands();
    assert!(matches!(
        commands.as_slice(),
        [WorkerCommand::RefreshConnectionModels { request, .. }]
            if request.request_id == model_request_id
    ));
    assert!(!app.has_pending_worker_commands());
    assert!(app.drain_pending_worker_commands().is_empty());

    let before_modal = app.modal_lines();
    let before_notice = app.last_notice().map(str::to_owned);
    app.handle_worker_message(WorkerMessage::ProviderModelsRefreshed {
        request_id: model_request_id,
        base_url: "https://wrong-origin.example/v1".to_owned(),
        result: Ok(vec!["wrong-origin-model".to_owned()]),
    })?;
    assert_eq!(app.modal_lines(), before_modal);
    assert_eq!(app.last_notice(), before_notice.as_deref());
    assert_eq!(
        app.runtime
            .active_model_picker_refresh
            .as_ref()
            .map(|pending| pending.request_id),
        Some(model_request_id)
    );

    app.handle_worker_message(WorkerMessage::ProviderModelsRefreshed {
        request_id: model_request_id + 1,
        base_url: model_base_url,
        result: Ok(vec!["stale".to_owned()]),
    })?;
    assert!(app.runtime.active_model_picker_refresh.is_some());

    app.runtime.active_model_picker_refresh = None;
    app.handle_worker_message(WorkerMessage::ProviderModelsRefreshed {
        request_id: model_request_id,
        base_url: "https://none.example".to_owned(),
        result: Ok(vec!["ignored".to_owned()]),
    })?;

    app.runtime.active_balance_refresh_id = Some(7);
    let previous_status = app.runtime.balance_snapshot.status.clone();
    app.handle_worker_message(WorkerMessage::ProviderBalanceRefreshed {
        request_id: 8,
        snapshot: sigil_runtime::BalanceSnapshot {
            total: Some(1.0),
            currency: Some("USD".to_owned()),
            available: true,
            status: "USD 1.00".to_owned(),
        },
    })?;
    assert_eq!(app.runtime.active_balance_refresh_id, Some(7));
    assert_eq!(app.runtime.balance_snapshot.status, previous_status);
    Ok(())
}

#[test]
fn schedule_balance_refresh_handles_missing_config_and_auth() {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::unset("SIGIL_API_KEY");
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.config_snapshot = None;
    app.schedule_balance_refresh();
    assert_eq!(app.runtime.balance_snapshot.status, "n/a");
    assert!(app.runtime.active_balance_refresh_id.is_none());

    app.apply_runtime_config_snapshot(&test_config());
    app.schedule_balance_refresh();
    assert_eq!(app.runtime.balance_snapshot.status, "missing auth");
    assert!(app.runtime.active_balance_refresh_id.is_none());

    let temp = tempdir().expect("tempdir should be created");
    let mut setup_app = AppState::from_setup(
        temp.path().join("sigil.toml"),
        temp.path().join("workspace"),
        None,
    );
    setup_app.schedule_balance_refresh();
    assert!(setup_app.runtime.active_balance_refresh_id.is_none());
}

#[test]
fn schedule_balance_refresh_skips_non_deepseek_provider() {
    let mut config = test_config();
    config.agent.runtime_provider = "custom".to_owned();
    config.agent.connection =
        Some(sigil_kernel::ConnectionId::new("openai-compatible").expect("connection id"));
    config.agent.model = "gpt-test".to_owned();
    config.connections.insert(
        "openai-compatible".to_owned(),
        json!({
            "label": "OpenAI compatible",
            "provider": "custom",
            "protocol": "chat_completions",
            "base_url": "https://openai.example.com/v1",
            "credential": {"source": "none"}
        }),
    );
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    app.schedule_balance_refresh();

    assert_eq!(app.runtime.balance_snapshot.status, "n/a");
    assert!(app.runtime.active_balance_refresh_id.is_none());
    assert!(
        !app.drain_pending_worker_commands()
            .iter()
            .any(|command| matches!(command, WorkerCommand::RefreshProviderBalance { .. }))
    );
}

#[test]
fn code_intelligence_results_update_status_lines_and_diagnostics() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-code-status",
        "code_status",
        "{}".to_owned(),
        ToolResultMeta {
            details: json!({
                "code_intelligence": {
                    "servers": [
                        { "server": "rust-analyzer", "status": "ready", "languages": ["rust"] },
                        { "server": "pyright", "status": "fallback", "languages": ["python"] }
                    ]
                }
            }),
            ..ToolResultMeta::default()
        },
    )))?;

    assert_eq!(app.runtime.code_intelligence_status, "ready");
    assert_eq!(
        app.runtime
            .code_intelligence_server_lines
            .get("rust-analyzer"),
        Some(&"rust: ready rust-analyzer".to_owned())
    );
    assert_eq!(
        app.runtime.code_intelligence_server_lines.get("pyright"),
        Some(&"python: fallback pyright".to_owned())
    );

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-code-diag",
        "code_diagnostics",
        json!({
            "query": { "paths": ["./src/main.rs", "src/lib.rs"] },
            "diagnostics": [
                { "path": "./src/main.rs", "severity": "error" },
                { "path": "src/main.rs", "severity": "warning" }
            ]
        })
        .to_string(),
        ToolResultMeta::default(),
    )))?;

    assert_eq!(
        app.runtime.code_intelligence_status,
        "diagnostics 1 errors 1 warnings"
    );
    assert_eq!(
        app.runtime.code_intelligence_diagnostics_line.as_deref(),
        Some("diagnostics: 1 errors 1 warnings")
    );
    assert_eq!(
        app.runtime
            .code_intelligence_diagnostics_by_path
            .get("src/main.rs"),
        Some(&ApprovalDiagnosticSummary {
            errors: 1,
            warnings: 1,
        })
    );
    assert_eq!(
        app.runtime
            .code_intelligence_diagnostics_by_path
            .get("src/lib.rs"),
        Some(&ApprovalDiagnosticSummary::default())
    );

    app.handle(RunEvent::ToolResult(ToolResult::error(
        "call-code-error",
        "code_search",
        ToolErrorKind::Protocol,
        "bad response",
    )))?;
    assert_eq!(app.runtime.code_intelligence_status, "degraded tool error");
    assert_eq!(
        app.runtime.code_intelligence_server_lines.get("status"),
        Some(&"status: degraded tool error".to_owned())
    );
    Ok(())
}

#[test]
fn worker_messages_cover_run_start_notice_and_manual_compaction_restore() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle_worker_message(WorkerMessage::RunStarted {
        prompt: "draft plan".to_owned(),
    })?;
    assert_eq!(app.run_phase(), RunPhase::Thinking);
    assert_eq!(app.last_notice(), Some("thinking"));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "run:start" && event.detail == "draft plan")
    );
    app.handle_worker_message(WorkerMessage::SkillRunStarted {
        skill_id: "repo-review".to_owned(),
        prompt: "load and apply skill".to_owned(),
    })?;
    assert_eq!(app.run_phase(), RunPhase::Thinking);
    assert_eq!(app.last_notice(), Some("skill repo-review running"));
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice && entry.text == "skill repo-review started"
    }));
    assert!(
        app.events.iter().any(|event| {
            event.label == "skill:start" && event.detail == "load and apply skill"
        })
    );
    app.handle_worker_message(WorkerMessage::PlanRunStarted {
        prompt: "inspect before editing".to_owned(),
    })?;
    assert_eq!(app.run_phase(), RunPhase::Thinking);
    assert_eq!(app.last_notice(), Some("planning"));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "plan:start" && event.detail == "inspect before editing")
    );
    app.handle_worker_message(WorkerMessage::AgentRunStarted {
        profile_id: "review".to_owned(),
        prompt: "inspect kernel".to_owned(),
    })?;
    assert_eq!(app.run_phase(), RunPhase::Agent("review".to_owned()));
    assert_eq!(app.last_notice(), Some("waiting for agent @review"));
    assert_eq!(
        app.live_activity_summary()
            .expect("agent run should expose live activity")
            .detail,
        "waiting for @review result"
    );
    assert!(!app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice
            && entry.text == "agent @review started; waiting for result"
    }));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "agent:start" && event.detail == "inspect kernel")
    );
    app.handle_worker_message(WorkerMessage::AgentResultContinuationStarted {
        thread_ids: vec![sigil_kernel::AgentThreadId::new("agent_chat_done")?],
    })?;
    assert!(app.runtime.is_busy);
    assert_eq!(app.run_phase(), RunPhase::Thinking);
    assert_eq!(app.last_notice(), Some("agent result ready; resuming main"));
    assert!(!app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice
            && entry
                .text
                .contains("agent result ready; resuming main for agent_chat_done")
    }));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "agent:resume" && event.detail == "agent_chat_done")
    );

    app.handle_worker_message(WorkerMessage::AgentRunFinished {
        profile_id: "review".to_owned(),
        result: sigil_kernel::AgentRunResult {
            final_text: "kernel review complete".to_owned(),
            tool_calls: 0,
            final_message_id: None,
        },
        entries: restored_entries("restored-provider", "restored-model"),
    })?;
    assert!(!app.runtime.is_busy);
    assert_eq!(app.run_phase(), RunPhase::Idle);
    assert_eq!(app.last_notice(), Some("agent @review finished"));
    assert!(!app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice && entry.text == "agent @review finished"
    }));
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Assistant && entry.text == "kernel review complete"
    }));
    assert!(app.events.iter().any(|event| {
        event.label == "agent:finish"
            && event
                .detail
                .contains("review tool_calls=0 final_text_bytes=22")
    }));

    let mut duplicate_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    duplicate_app.handle_worker_message(WorkerMessage::AgentRunFinished {
        profile_id: "review".to_owned(),
        result: sigil_kernel::AgentRunResult {
            final_text: "restored final".to_owned(),
            tool_calls: 1,
            final_message_id: None,
        },
        entries: vec![
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name: "restored-provider".to_owned(),
                model_name: "restored-model".to_owned(),
                resolved_model_route: None,
            }),
            SessionLogEntry::User(ModelMessage::user("prompt")),
            SessionLogEntry::Assistant(ModelMessage::assistant_with_kind(
                Some("restored final".to_owned()),
                Vec::new(),
                sigil_kernel::AssistantMessageKind::FinalAnswer,
            )),
            v2_tool_result_entry(
                "call-1",
                "test_tool",
                "post-final fact",
                ToolResultMeta::default(),
            ),
        ],
    })?;
    assert_eq!(
        duplicate_app
            .timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant && entry.text == "restored final")
            .count(),
        1
    );

    app.handle_worker_message(WorkerMessage::McpActivationStatus {
        server_name: None,
        status: McpActivationStatus::Deferred,
    })?;
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "mcp" && event.detail == "deferred")
    );

    Ok(())
}

#[test]
fn worker_messages_cover_task_start_and_all_finish_status_labels() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle_worker_message(WorkerMessage::TaskRunStarted {
        task_id: "task_1".to_owned(),
        objective: "ship task".to_owned(),
    })?;

    assert_eq!(app.run_phase(), RunPhase::Thinking);
    assert_eq!(app.last_notice(), Some("planning task task_1"));
    assert!(
        app.events
            .iter()
            .any(|event| { event.label == "task:start" && event.detail == "task_1 ship task" })
    );

    for (status, label) in [
        (sigil_kernel::TaskRunStatus::Started, "started"),
        (sigil_kernel::TaskRunStatus::Running, "running"),
        (sigil_kernel::TaskRunStatus::Paused, "paused"),
        (sigil_kernel::TaskRunStatus::Completed, "completed"),
        (sigil_kernel::TaskRunStatus::Failed, "failed"),
        (sigil_kernel::TaskRunStatus::Cancelled, "cancelled"),
        (sigil_kernel::TaskRunStatus::Interrupted, "interrupted"),
    ] {
        app.runtime.is_busy = true;
        app.handle_worker_message(WorkerMessage::TaskRunFinished {
            task_id: "task_1".to_owned(),
            status,
            entries: Vec::new(),
        })?;

        assert!(!app.runtime.is_busy);
        assert_eq!(app.run_phase(), RunPhase::Idle);
        let expected_notice = format!("task task_1 {label}");
        assert_eq!(app.last_notice(), Some(expected_notice.as_str()));
        assert!(app.events.iter().any(|event| {
            event.label == "task:finish" && event.detail == format!("task_1 status={label}")
        }));
    }

    app.runtime.is_busy = true;
    app.handle_worker_message(WorkerMessage::TaskRunFinished {
        task_id: "task_1".to_owned(),
        status: sigil_kernel::TaskRunStatus::Failed,
        entries: vec![sigil_kernel::SessionLogEntry::Control(
            sigil_kernel::ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
                task_id: sigil_kernel::TaskId::new("task_1")?,
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                objective: "ship task".to_owned(),
                title: None,
                status: sigil_kernel::TaskRunStatus::Failed,
                reason: Some("step gate_check failed".to_owned()),
            }),
        )],
    })?;
    assert!(!app.runtime.is_busy);
    assert_eq!(
        app.last_notice(),
        Some("task task_1 failed: step gate_check failed")
    );
    Ok(())
}

#[test]
fn worker_control_events_update_task_sidebar_immediately() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("overview")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::Control(ControlEntry::TaskRun(
        sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            title: None,
            status: sigil_kernel::TaskRunStatus::Running,
            reason: Some("continuing plan v1".to_owned()),
        },
    )))?;
    app.handle(RunEvent::Control(ControlEntry::TaskPlan(
        sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: step_id.clone(),
                title: "scan workspace".to_owned(),
                display_name: None,
                detail: None,
                role: sigil_kernel::AgentRole::Executor,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        },
    )))?;
    app.handle(RunEvent::Control(ControlEntry::TaskStep(
        sigil_kernel::TaskStepEntry {
            task_id,
            plan_version: 1,
            step_id,
            role: sigil_kernel::AgentRole::Executor,
            status: sigil_kernel::TaskStepStatus::Running,
            title: Some("scan workspace".to_owned()),
            summary: None,
            reason: None,
        },
    )))?;

    let lines = app.task_sidebar_lines();

    assert!(lines.contains(&"status: running".to_owned()));
    assert!(lines.contains(&"current: v1:overview running".to_owned()));
    assert!(lines.contains(&"◐ 1. running overview · scan workspace".to_owned()));
    Ok(())
}

#[test]
fn worker_messages_cover_run_finished_notice_session_switch_and_failure_reset() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        config_version: 2,
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let restored_path = temp.path().join("session-restored.jsonl");
    let entries = restored_entries("restored-provider", "restored-model");

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.runtime.is_busy = true;
    app.approval.pending = Some(PendingApproval {
        approval_request_id: "approval-1".to_owned(),
        call: ToolCall {
            id: "call-1".to_owned(),
            name: "write_file".to_owned(),
            args_json: "{}".to_owned(),
        },
        spec: ToolSpec {
            name: "write_file".to_owned(),
            description: "Write a file".to_owned(),
            input_schema: json!({"type": "object"}),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            network_effect: None,
            preview: ToolPreviewCapability::Required,
        },
        effects: std::collections::BTreeSet::new(),
        subjects: Vec::new(),
        analysis: sigil_kernel::ToolAnalysisStatus::Complete,
        containment: sigil_kernel::ExecutionContainmentRequest::default(),
        safe_summary: sigil_kernel::ToolPermissionSummary::default(),
        decision_reasons: Vec::new(),
        network_effect: None,
        local_policy_decision: sigil_kernel::ApprovalMode::Ask,
        network_policy_decision: sigil_kernel::ApprovalMode::Allow,
        source_policy_decision: sigil_kernel::ApprovalMode::Allow,
        operation: sigil_kernel::ToolOperation::OverwriteFile,
        risk: sigil_kernel::PermissionRisk::Medium,
        subject_zones: Vec::new(),
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
        session_grant_available: false,
        session_grant_unavailable_reason: Some(sigil_kernel::ToolApprovalSessionGrantUnavailableReason {
            code: sigil_kernel::ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
        }),
        command_family_allow_pattern: None,
        preview: None,
        presentation_state: crate::app::ApprovalPresentationState::Pending,
    });
    app.modal_state = Some(ModalState::KeyboardHelp);
    app.timeline_state.streaming_reasoning_index = Some(0);

    app.handle_worker_message(WorkerMessage::RunFinished {
        result: sigil_kernel::AgentRunResult {
            final_text: "done".to_owned(),
            tool_calls: 2,
            final_message_id: None,
        },
        entries: entries.clone(),
    })?;

    assert!(!app.runtime.is_busy);
    assert!(app.approval.pending.is_none());
    assert!(app.modal_state.is_none());
    assert_eq!(app.run_phase(), RunPhase::Idle);
    assert_eq!(app.last_notice(), Some("agent idle"));
    assert!(app.events.iter().any(|event| {
        event.label == "run:finish" && event.detail == "tool_calls=2 final_text_bytes=4"
    }));

    app.handle_worker_message(WorkerMessage::Notice("worker note".to_owned()))?;
    assert_eq!(app.last_notice(), Some("worker note"));
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "worker note")
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "worker" && event.detail == "worker note")
    );
    app.runtime.mutation_artifact_retention_preview = MutationArtifactRetentionPreview::Pending;
    app.handle_worker_message(WorkerMessage::Notice(
        "mutation artifact cleanup: scanned 0 artifacts (0 bytes), expired 0, deleted 0, unavailable 0, recorded 0 lifecycle events".to_owned(),
    ))?;
    assert_eq!(
        app.last_notice(),
        Some(
            "mutation artifact cleanup: scanned 0 artifacts (0 bytes), expired 0, deleted 0, unavailable 0, recorded 0 lifecycle events"
        )
    );
    assert!(matches!(
        app.runtime.mutation_artifact_retention_preview,
        MutationArtifactRetentionPreview::Ready { .. }
            | MutationArtifactRetentionPreview::Unavailable(_)
    ));
    app.handle_worker_message(WorkerMessage::Notice("worker failed hard".to_owned()))?;
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "worker failed hard")
    );

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path: restored_path.clone(),
        provider_name: "restored-provider".to_owned(),
        model_name: "restored-model".to_owned(),
        entries: entries.clone(),
    })?;
    assert_eq!(app.session_log_path, restored_path);
    assert_eq!(app.runtime.provider_name, "restored-provider");
    assert_eq!(app.runtime.model_name, "restored-model");
    assert_eq!(app.last_notice(), Some("restored from disk"));

    app.runtime.is_busy = true;
    app.modal_state = Some(ModalState::KeyboardHelp);
    app.handle_worker_message(WorkerMessage::RunFailed(
        "request failed\n\nCaused by:\n  0: timeout".to_owned(),
    ))?;
    assert!(!app.runtime.is_busy);
    assert!(app.modal_state.is_none());
    assert_eq!(app.run_phase(), RunPhase::Idle);
    assert_eq!(app.last_notice(), Some("timeout"));
    Ok(())
}

#[test]
fn exact_approval_receipt_hides_actions_and_projects_resuming_state() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.runtime.is_busy = true;
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    app.handle_worker_message(WorkerMessage::ApprovalCommandReceipt(
        crate::runner::WorkerApprovalCommandReceipt {
            command_id: "command-approve".to_owned(),
            approval_request_id: "approval-call-1".to_owned(),
            call_id: "call-1".to_owned(),
            decision: crate::runner::WorkerApprovalDecision::ApproveOnce,
            route_state: crate::runner::WorkerApprovalRouteState::DecisionAccepted,
            replayed: false,
        },
    ))?;

    let pending = app.approval.pending.as_ref().expect("accepted tombstone");
    assert!(matches!(
        pending.presentation_state,
        crate::app::ApprovalPresentationState::DecisionAccepted { .. }
    ));
    assert!(app.approval_modal_view().is_none());
    assert_eq!(
        app.live_activity_summary().map(|summary| summary.detail),
        Some("decision accepted for write_file; resuming run".to_owned())
    );
    Ok(())
}

#[test]
fn exact_resolution_clears_an_accepted_approval_tombstone_idempotently() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    app.handle_worker_message(WorkerMessage::ApprovalCommandReceipt(
        crate::runner::WorkerApprovalCommandReceipt {
            command_id: "command-approve".to_owned(),
            approval_request_id: "approval-call-1".to_owned(),
            call_id: "call-1".to_owned(),
            decision: crate::runner::WorkerApprovalDecision::ApproveOnce,
            route_state: crate::runner::WorkerApprovalRouteState::DecisionAccepted,
            replayed: false,
        },
    ))?;

    let resolved = RunEvent::ToolApprovalResolved {
        call_id: "call-1".to_owned(),
        approval_request_id: "approval-call-1".to_owned(),
        approved: true,
        reason: None,
    };
    app.handle(resolved.clone())?;
    assert!(app.approval.pending.is_none());

    app.handle(resolved)?;
    assert!(app.approval.pending.is_none());
    assert_eq!(app.run_phase(), RunPhase::Thinking);
    Ok(())
}

#[test]
fn stale_approval_receipt_and_resolution_do_not_close_newer_request() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    app.handle_worker_message(WorkerMessage::ApprovalCommandReceipt(
        crate::runner::WorkerApprovalCommandReceipt {
            command_id: "command-old".to_owned(),
            approval_request_id: "approval-old".to_owned(),
            call_id: "call-1".to_owned(),
            decision: crate::runner::WorkerApprovalDecision::ApproveOnce,
            route_state: crate::runner::WorkerApprovalRouteState::DecisionAccepted,
            replayed: true,
        },
    ))?;
    assert!(app.approval.pending.as_ref().is_some_and(|pending| {
        pending.approval_request_id == "approval-call-1"
            && matches!(
                pending.presentation_state,
                crate::app::ApprovalPresentationState::Pending
            )
    }));

    app.handle(RunEvent::ToolApprovalResolved {
        call_id: "call-1".to_owned(),
        approval_request_id: "approval-old".to_owned(),
        approved: true,
        reason: None,
    })?;
    assert!(app.approval.pending.is_some());
    Ok(())
}

#[test]
fn uncertain_approval_delivery_is_non_actionable_until_authority_resolves() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    app.handle_worker_message(WorkerMessage::ApprovalCommandReceipt(
        crate::runner::WorkerApprovalCommandReceipt {
            command_id: "command-uncertain".to_owned(),
            approval_request_id: "approval-call-1".to_owned(),
            call_id: "call-1".to_owned(),
            decision: crate::runner::WorkerApprovalDecision::ApproveOnce,
            route_state: crate::runner::WorkerApprovalRouteState::DeliveryUncertain,
            replayed: false,
        },
    ))?;

    assert!(matches!(
        app.approval
            .pending
            .as_ref()
            .expect("uncertain tombstone")
            .presentation_state,
        crate::app::ApprovalPresentationState::DeliveryUncertain { .. }
    ));
    assert!(app.approval_modal_view().is_none());
    assert!(
        app.live_activity_summary()
            .is_some_and(|summary| summary.detail.contains("approval state uncertain"))
    );
    Ok(())
}

#[test]
fn run_finished_does_not_duplicate_visible_final_answer_or_drop_thinking() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta(
        "draft summary that should stay visible".to_owned(),
    ))?;
    app.handle(RunEvent::AssistantMessage(
        ModelMessage::assistant_with_kind(
            Some("final summary".to_owned()),
            Vec::new(),
            sigil_kernel::AssistantMessageKind::FinalAnswer,
        ),
    ))?;
    app.handle_worker_message(WorkerMessage::RunFinished {
        result: sigil_kernel::AgentRunResult {
            final_text: "final summary".to_owned(),
            tool_calls: 0,
            final_message_id: None,
        },
        entries: Vec::new(),
    })?;

    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant && entry.text == "final summary")
            .count(),
        1
    );
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Thinking)
            .count(),
        1
    );
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Thinking
            && entry
                .text
                .contains("draft summary that should stay visible")
    }));
    Ok(())
}

#[test]
fn plain_assistant_message_keeps_intermediate_text_boundary() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta(
        "analysis that belongs to the in-flight turn".to_owned(),
    ))?;
    app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
        Some("intermediate status".to_owned()),
        Vec::new(),
    )))?;

    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Thinking
                && entry.text.contains("analysis that belongs"))
    );
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Assistant
                && entry.text == "intermediate status")
    );
    Ok(())
}

#[test]
fn worker_events_cover_completion_continuation_and_duplicate_assistant_messages() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ToolCallArgsDelta {
        id: "call-1".to_owned(),
        delta: "{}".to_owned(),
    })?;
    assert_eq!(app.run_phase(), RunPhase::Tool("tool".to_owned()));

    app.handle(RunEvent::ToolCallCompleted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    assert_eq!(app.run_phase(), RunPhase::Tool("read_file".to_owned()));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "tool:complete" && event.detail == "read_file call-1")
    );
    app.handle(RunEvent::ToolCallCompleted(ToolCall {
        id: "call-agent".to_owned(),
        name: "wait_agent".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    assert_eq!(app.run_phase(), RunPhase::Tool("wait_agent".to_owned()));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "tool:complete" && event.detail == "wait_agent call-agent")
    );

    app.handle(RunEvent::Control(ControlEntry::Note {
        kind: "custom".to_owned(),
        data: json!({ "value": 1 }),
    }))?;
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "control" && event.detail.contains("custom"))
    );

    app.handle(RunEvent::ContinuationState(
        sigil_kernel::ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "resume".to_owned(),
            message_id: Some("msg-1".to_owned()),
            opaque_blob: json!({ "cursor": 1 }),
        },
    ))?;
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "continuation" && event.detail == "resume")
    );

    app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
        Some("same answer".to_owned()),
        Vec::new(),
    )))?;
    app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
        Some("same answer".to_owned()),
        Vec::new(),
    )))?;
    app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
        Some(String::new()),
        Vec::new(),
    )))?;

    let matching = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Assistant && entry.text == "same answer")
        .count();
    assert_eq!(matching, 1);
    Ok(())
}

#[test]
fn assistant_tool_preamble_becomes_thinking_before_one_final_reply() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.runtime.is_busy = true;

    let progress_text = "check 通过。跑相关 crate 的测试。";
    app.handle(RunEvent::TextDelta(progress_text.to_owned()))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-check".to_owned(),
        name: "bash".to_owned(),
        args_json: r#"{"command":"cargo check"}"#.to_owned(),
    }))?;
    app.handle(RunEvent::AssistantMessage(
        ModelMessage::assistant_with_kind(
            Some(progress_text.to_owned()),
            vec![ToolCall {
                id: "call-check".to_owned(),
                name: "bash".to_owned(),
                args_json: r#"{"command":"cargo check"}"#.to_owned(),
            }],
            AssistantMessageKind::ToolPreamble,
        ),
    ))?;

    let preamble_replies = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Assistant && entry.text == progress_text)
        .count();
    assert_eq!(preamble_replies, 0);
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Thinking && entry.text == progress_text)
    );
    assert_eq!(app.run_phase(), RunPhase::Tool("bash".to_owned()));
    let summary = app
        .live_activity_summary()
        .expect("expected live tool activity");
    assert_eq!(summary.label, "tool");
    assert_eq!(summary.detail, "running bash");

    app.handle(RunEvent::TextDelta("done".to_owned()))?;
    app.handle(RunEvent::AssistantMessage(
        ModelMessage::assistant_with_kind(
            Some("done".to_owned()),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        ),
    ))?;
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        1
    );

    app.handle(RunEvent::Control(ControlEntry::ToolExecution(Box::new(
        ToolExecutionEntry {
            call_id: "call-test".to_owned(),
            tool_name: "cargo test".to_owned(),
            status: ToolExecutionStatus::Started,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        },
    ))))?;
    assert_eq!(app.run_phase(), RunPhase::Tool("cargo test".to_owned()));
    Ok(())
}

#[test]
fn agent_thread_event_updates_only_focused_child_transcript() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let thread_id = sigil_kernel::AgentThreadId::new("agent_chat_live")?;

    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::TextDelta("main ignored".to_owned())),
    })?;
    assert!(app.agent_panel.active_child_transcript.is_none());

    app.agent_panel.active_view = super::super::AgentView::Child {
        child_task_id: thread_id.as_str().to_owned(),
        child_session_ref: sigil_kernel::SessionRef::new_relative(
            "children/agent_chat_live.jsonl",
        )?,
    };
    app.agent_panel.active_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: Path::new("children/agent_chat_live.jsonl").to_path_buf(),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries: Vec::new(),
        rendered_body_lines: Vec::new(),
        total_timeline_entries: 0,
        transcript_truncated: false,
        load_error: Some("not written yet".to_owned()),
    });

    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::TextDelta("hel".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id,
        event: Box::new(RunEvent::TextDelta("lo".to_owned())),
    })?;

    let transcript = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("focused child transcript should exist");
    assert_eq!(transcript.timeline_entries.len(), 1);
    assert_eq!(transcript.timeline_entries[0].role, TimelineRole::Assistant);
    assert_eq!(transcript.timeline_entries[0].text, "hello");
    assert!(transcript.load_error.is_none());
    assert!(!transcript.rendered_body_lines.is_empty());

    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: sigil_kernel::AgentThreadId::new("agent_chat_other")?,
        event: Box::new(RunEvent::Notice("ignore me".to_owned())),
    })?;
    let transcript = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("focused child transcript should remain loaded");
    assert_eq!(transcript.timeline_entries.len(), 1);
    assert!(
        !transcript
            .timeline_entries
            .iter()
            .any(|entry| entry.text.contains("ignore me"))
    );
    Ok(())
}

fn focus_live_child_transcript(
    app: &mut AppState,
    thread_id: &sigil_kernel::AgentThreadId,
) -> Result<()> {
    let relative_path = format!("children/{}.jsonl", thread_id.as_str());
    app.agent_panel.active_view = super::super::AgentView::Child {
        child_task_id: thread_id.as_str().to_owned(),
        child_session_ref: sigil_kernel::SessionRef::new_relative(relative_path.clone())?,
    };
    app.agent_panel.active_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: Path::new(&relative_path).to_path_buf(),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries: Vec::new(),
        rendered_body_lines: Vec::new(),
        total_timeline_entries: 0,
        transcript_truncated: false,
        load_error: None,
    });
    Ok(())
}

#[test]
fn child_bash_progress_and_final_share_one_redacted_command_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.secret_redactor = sigil_kernel::SecretRedactor::from_values(["child-secret"]);
    let thread_id = sigil_kernel::AgentThreadId::new("agent_child_bash_live")?;
    let call_id = "call-child-bash";
    focus_live_child_transcript(&mut app, &thread_id)?;

    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolCallStarted(ToolCall {
            id: call_id.to_owned(),
            name: "bash".to_owned(),
            args_json: json!({"command":"printf exact-provider-argument"}).to_string(),
        })),
    })?;
    let pending = &app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("child pending transcript")
        .timeline_entries[0]
        .text;
    let pending_payload: serde_json::Value = serde_json::from_str(pending)?;
    assert_eq!(pending_payload["status"], "running");
    assert!(pending.contains("bash is running"));
    assert!(!pending.contains("exact-provider-argument"));
    assert!(app.agent_panel.safe_child_tool_calls.is_empty());

    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolCallCompleted(ToolCall {
            id: call_id.to_owned(),
            name: "bash".to_owned(),
            args_json: json!({
                "command": "printf child-secret && cargo check --workspace"
            })
            .to_string(),
        })),
    })?;
    let completed_pending = &app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("completed safe call should retain pending card")
        .timeline_entries[0]
        .text;
    let completed_pending_payload: serde_json::Value = serde_json::from_str(completed_pending)?;
    assert_eq!(completed_pending_payload["status"], "running");
    assert_eq!(
        completed_pending_payload["metadata"]["details"]["call"]["summary"],
        "command=printf [redacted] && cargo check --workspace"
    );
    let completed_pending_rendered = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("completed safe call should render pending card")
        .rendered_body_lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(completed_pending_rendered.contains("RUNNING"));
    assert!(!completed_pending_rendered.contains(" OK "));
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolProgress(sigil_kernel::ToolProgressEvent {
            execution_id: sigil_kernel::ToolExecutionId::new("child-bash-execution")?,
            call_id: call_id.to_owned(),
            tool_name: "bash".to_owned(),
            sequence: 1,
            status: "running".to_owned(),
            message: Some("foreground shell command is running".to_owned()),
            output_preview: None,
            output_log_ref: None,
            total_bytes: Some(0),
            updated_at_ms: None,
            details: json!({"execution_mode":"foreground"}),
        })),
    })?;
    let progress_transcript = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("child progress transcript");
    assert_eq!(progress_transcript.timeline_entries.len(), 1);
    let progress_payload: serde_json::Value =
        serde_json::from_str(&progress_transcript.timeline_entries[0].text)?;
    assert_eq!(progress_payload["status"], "running");
    assert_eq!(
        progress_payload["metadata"]["details"]["execution_id"],
        "child-bash-execution"
    );
    assert_eq!(
        progress_payload["metadata"]["details"]["call"]["summary"],
        "command=printf [redacted] && cargo check --workspace"
    );
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolResult(production_display_bash_result(
            call_id,
            "child check completed",
        ))),
    })?;

    let transcript = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("focused child transcript");
    let cards = transcript
        .timeline_entries
        .iter()
        .filter(|entry| entry.role == TimelineRole::Tool)
        .collect::<Vec<_>>();
    assert_eq!(cards.len(), 1);
    let payload: serde_json::Value = serde_json::from_str(&cards[0].text)?;
    assert_eq!(payload["status"], "ok");
    assert_eq!(
        payload["metadata"]["details"]["call"]["summary"],
        "command=printf [redacted] && cargo check --workspace"
    );
    assert_eq!(
        payload["metadata"]["details"]["execution_id"],
        "child-bash-execution"
    );
    assert!(cards[0].text.contains("child check completed"));
    assert!(
        !cards[0]
            .text
            .contains("foreground shell command is running")
    );
    assert!(!cards[0].text.contains("child-secret"));
    assert!(app.agent_panel.safe_child_tool_calls.is_empty());
    assert!(app.agent_panel.child_tool_progress_execution_ids.is_empty());
    assert!(app.agent_panel.child_tool_card_entry_indices.is_empty());
    Ok(())
}

#[test]
fn child_completed_call_restores_command_after_switch_before_progress_and_final() -> Result<()> {
    let temp = tempdir()?;
    let thread_id = sigil_kernel::AgentThreadId::new("agent_child_bash_switch")?;
    let child_session_ref =
        sigil_kernel::SessionRef::new_relative("children/agent_child_bash_switch.jsonl")?;
    let child_path = temp.path().join(child_session_ref.as_path());
    let child_store = JsonlSessionStore::new(&child_path)?;
    let call_id = "call-child-bash-switch";
    let safe_call = ToolCall {
        id: call_id.to_owned(),
        name: "bash".to_owned(),
        args_json: json!({"command":"printf restored-child-command"}).to_string(),
    };

    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &test_config());
    app.session_log_path = temp.path().join("parent.jsonl");
    app.agent_panel.active_view = super::super::AgentView::Child {
        child_task_id: thread_id.as_str().to_owned(),
        child_session_ref: child_session_ref.clone(),
    };
    app.agent_panel.active_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: child_path.clone(),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries: Vec::new(),
        rendered_body_lines: Vec::new(),
        total_timeline_entries: 0,
        transcript_truncated: false,
        load_error: None,
    });

    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolCallCompleted(safe_call.clone())),
    })?;
    child_store.append(&SessionLogEntry::Assistant(
        ModelMessage::assistant_with_kind(
            None,
            vec![safe_call],
            AssistantMessageKind::ToolPreamble,
        ),
    ))?;

    app.agent_panel.active_view = super::super::AgentView::Main;
    assert!(app.reload_active_agent_child_transcript());
    assert!(app.agent_panel.safe_child_tool_calls.is_empty());
    app.agent_panel.active_view = super::super::AgentView::Child {
        child_task_id: thread_id.as_str().to_owned(),
        child_session_ref: child_session_ref.clone(),
    };
    assert!(app.reload_active_agent_child_transcript());

    let restored = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("restored child transcript");
    assert_eq!(restored.timeline_entries.len(), 1);
    assert!(
        restored.timeline_entries[0]
            .text
            .contains("status\":\"running")
    );
    assert!(
        restored.timeline_entries[0]
            .text
            .contains("command=printf restored-child-command")
    );
    assert_eq!(app.agent_panel.safe_child_tool_calls.len(), 1);
    assert_eq!(app.agent_panel.child_tool_card_entry_indices.len(), 1);

    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolProgress(sigil_kernel::ToolProgressEvent {
            execution_id: sigil_kernel::ToolExecutionId::new("child-switch-execution")?,
            call_id: call_id.to_owned(),
            tool_name: "bash".to_owned(),
            sequence: 1,
            status: "running".to_owned(),
            message: Some("restored child command is running".to_owned()),
            output_preview: None,
            output_log_ref: None,
            total_bytes: Some(0),
            updated_at_ms: Some(1),
            details: json!({"execution_mode":"foreground"}),
        })),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolResult(production_display_bash_result(
            call_id,
            "restored child done",
        ))),
    })?;

    let transcript = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("completed restored child transcript");
    let cards = transcript
        .timeline_entries
        .iter()
        .filter(|entry| entry.role == TimelineRole::Tool)
        .collect::<Vec<_>>();
    assert_eq!(cards.len(), 1);
    assert!(
        cards[0]
            .text
            .contains("command=printf restored-child-command")
    );
    assert!(cards[0].text.contains("restored child done"));
    assert!(cards[0].text.contains("child-switch-execution"));
    assert!(app.agent_panel.child_tool_card_entry_indices.is_empty());
    Ok(())
}

#[test]
fn child_failed_audit_then_live_result_replaces_one_restored_occurrence() -> Result<()> {
    let temp = tempdir()?;
    let thread_id = sigil_kernel::AgentThreadId::new("agent_child_failed_audit_race")?;
    let child_session_ref =
        sigil_kernel::SessionRef::new_relative("children/agent_child_failed_audit_race.jsonl")?;
    let child_path = temp.path().join(child_session_ref.as_path());
    let child_store = JsonlSessionStore::new(&child_path)?;
    let call_id = "call-child-failed-audit-race";
    let safe_call = ToolCall {
        id: call_id.to_owned(),
        name: "bash".to_owned(),
        args_json: json!({"command":"cargo test -p sigil-tui"}).to_string(),
    };
    child_store.append(&SessionLogEntry::Assistant(
        ModelMessage::assistant_with_kind(
            None,
            vec![safe_call],
            AssistantMessageKind::ToolPreamble,
        ),
    ))?;
    child_store.append(&SessionLogEntry::Control(ControlEntry::ToolExecution(
        Box::new(ToolExecutionEntry {
            call_id: call_id.to_owned(),
            tool_name: "bash".to_owned(),
            status: ToolExecutionStatus::Failed,
            duration_ms: Some(1),
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        }),
    )))?;

    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &test_config());
    app.session_log_path = temp.path().join("parent.jsonl");
    app.agent_panel.active_view = super::super::AgentView::Child {
        child_task_id: thread_id.as_str().to_owned(),
        child_session_ref: child_session_ref.clone(),
    };
    assert!(app.reload_active_agent_child_transcript());

    let restored = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("failed audit child transcript");
    assert_eq!(restored.timeline_entries.len(), 1);
    assert_eq!(restored.total_timeline_entries, 1);
    assert!(
        restored.timeline_entries[0]
            .text
            .contains("status\":\"error")
    );
    assert_eq!(
        app.agent_panel
            .child_tool_card_entry_indices
            .values()
            .map(|occurrence| occurrence.entry_index())
            .collect::<Vec<_>>(),
        vec![Some(0)]
    );

    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolResult(ToolResult::error(
            call_id,
            "bash",
            ToolErrorKind::ExitStatus,
            "child final failure",
        ))),
    })?;

    let transcript = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("settled failed child transcript");
    assert_eq!(transcript.timeline_entries.len(), 1);
    assert_eq!(transcript.total_timeline_entries, 1);
    assert!(
        transcript.timeline_entries[0]
            .text
            .contains("child final failure")
    );
    assert!(
        transcript.timeline_entries[0]
            .text
            .contains("command=cargo test -p sigil-tui")
    );
    assert!(app.agent_panel.child_tool_card_entry_indices.is_empty());
    assert!(app.agent_panel.safe_child_tool_calls.is_empty());

    let mut replacement_app =
        AppState::from_root_config(&temp.path().join("sigil.toml"), &test_config());
    replacement_app.session_log_path = temp.path().join("replacement-parent.jsonl");
    replacement_app.agent_panel.active_view = super::super::AgentView::Child {
        child_task_id: thread_id.as_str().to_owned(),
        child_session_ref,
    };
    assert!(replacement_app.reload_active_agent_child_transcript());
    replacement_app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolCallStarted(ToolCall {
            id: call_id.to_owned(),
            name: "bash".to_owned(),
            args_json: json!({"command":"provider-exact-new-turn"}).to_string(),
        })),
    })?;
    replacement_app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolCallCompleted(ToolCall {
            id: call_id.to_owned(),
            name: "bash".to_owned(),
            args_json: json!({"command":"printf new-child-turn"}).to_string(),
        })),
    })?;
    replacement_app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id,
        event: Box::new(RunEvent::ToolResult(production_display_bash_result(
            call_id,
            "new child turn completed",
        ))),
    })?;
    let replacement_transcript = replacement_app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("replacement child transcript");
    assert_eq!(replacement_transcript.timeline_entries.len(), 2);
    assert_eq!(replacement_transcript.total_timeline_entries, 2);
    assert!(
        replacement_transcript.timeline_entries[0]
            .text
            .contains("status\":\"error")
    );
    assert!(
        replacement_transcript.timeline_entries[1]
            .text
            .contains("command=printf new-child-turn")
    );
    assert!(
        replacement_transcript.timeline_entries[1]
            .text
            .contains("new child turn completed")
    );
    Ok(())
}

#[test]
fn child_safe_tool_calls_with_reused_ids_are_isolated_by_thread() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let first_thread = sigil_kernel::AgentThreadId::new("agent_child_scope_first")?;
    let second_thread = sigil_kernel::AgentThreadId::new("agent_child_scope_second")?;
    let call_id = "provider-reused-child-call-id";

    focus_live_child_transcript(&mut app, &first_thread)?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: first_thread.clone(),
        event: Box::new(RunEvent::ToolCallCompleted(ToolCall {
            id: call_id.to_owned(),
            name: "bash".to_owned(),
            args_json: json!({"command":"printf first-child-command"}).to_string(),
        })),
    })?;

    focus_live_child_transcript(&mut app, &second_thread)?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: second_thread.clone(),
        event: Box::new(RunEvent::ToolCallCompleted(ToolCall {
            id: call_id.to_owned(),
            name: "bash".to_owned(),
            args_json: json!({"command":"printf second-child-command"}).to_string(),
        })),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: second_thread.clone(),
        event: Box::new(RunEvent::ToolResult(production_display_bash_result(
            call_id,
            "second child done",
        ))),
    })?;

    let transcript = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("second child transcript");
    assert_eq!(transcript.timeline_entries.len(), 1);
    assert!(
        transcript.timeline_entries[0]
            .text
            .contains("command=printf second-child-command")
    );
    assert!(
        !transcript.timeline_entries[0]
            .text
            .contains("first-child-command")
    );
    assert!(app.agent_panel.safe_child_tool_calls.is_empty());
    Ok(())
}

#[test]
fn child_reused_call_id_keeps_distinct_terminal_cards() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let thread_id = sigil_kernel::AgentThreadId::new("agent_child_reused_call")?;
    let call_id = "provider-reused-child-call-id";
    let execution_id = "provider-reused-child-execution-id";
    focus_live_child_transcript(&mut app, &thread_id)?;

    for (turn_index, (command, output)) in [
        ("printf first-child-command", "first child done"),
        ("printf second-child-command", "second child done"),
    ]
    .into_iter()
    .enumerate()
    {
        app.handle_worker_message(WorkerMessage::AgentThreadEvent {
            thread_id: thread_id.clone(),
            event: Box::new(RunEvent::ToolCallCompleted(ToolCall {
                id: call_id.to_owned(),
                name: "bash".to_owned(),
                args_json: json!({"command":command}).to_string(),
            })),
        })?;
        app.handle_worker_message(WorkerMessage::AgentThreadEvent {
            thread_id: thread_id.clone(),
            event: Box::new(RunEvent::ToolProgress(sigil_kernel::ToolProgressEvent {
                execution_id: sigil_kernel::ToolExecutionId::new(execution_id)?,
                call_id: call_id.to_owned(),
                tool_name: "bash".to_owned(),
                sequence: 1,
                status: "running".to_owned(),
                message: Some(format!("turn {turn_index} progress one")),
                output_preview: None,
                output_log_ref: None,
                total_bytes: Some(0),
                updated_at_ms: Some(1),
                details: json!({"execution_mode":"foreground"}),
            })),
        })?;
        app.handle_worker_message(WorkerMessage::AgentThreadEvent {
            thread_id: thread_id.clone(),
            event: Box::new(RunEvent::ToolProgress(sigil_kernel::ToolProgressEvent {
                execution_id: sigil_kernel::ToolExecutionId::new(execution_id)?,
                call_id: call_id.to_owned(),
                tool_name: "bash".to_owned(),
                sequence: 2,
                status: "running".to_owned(),
                message: Some(format!("turn {turn_index} progress two")),
                output_preview: None,
                output_log_ref: None,
                total_bytes: Some(0),
                updated_at_ms: Some(2),
                details: json!({"execution_mode":"foreground"}),
            })),
        })?;
        assert_eq!(
            app.agent_panel
                .active_child_transcript
                .as_ref()
                .expect("child progress transcript")
                .timeline_entries
                .iter()
                .filter(|entry| entry.role == TimelineRole::Tool)
                .count(),
            turn_index + 1,
            "progress must replace only the current child occurrence"
        );
        app.handle_worker_message(WorkerMessage::AgentThreadEvent {
            thread_id: thread_id.clone(),
            event: Box::new(RunEvent::ToolResult(production_display_bash_result(
                call_id, output,
            ))),
        })?;
    }

    let transcript = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("child transcript");
    let cards = transcript
        .timeline_entries
        .iter()
        .filter(|entry| entry.role == TimelineRole::Tool)
        .collect::<Vec<_>>();
    assert_eq!(cards.len(), 2);
    assert!(cards[0].text.contains("command=printf first-child-command"));
    assert!(cards[0].text.contains("first child done"));
    assert!(cards[0].text.contains(execution_id));
    assert!(!cards[0].text.contains("turn 1 progress"));
    assert!(
        cards[1]
            .text
            .contains("command=printf second-child-command")
    );
    assert!(cards[1].text.contains("second child done"));
    assert!(cards[1].text.contains(execution_id));
    assert!(!cards[1].text.contains("turn 0 progress"));
    assert!(app.agent_panel.child_tool_card_entry_indices.is_empty());
    Ok(())
}

#[test]
fn child_maximum_batch_finalizes_offscreen_pending_without_inflating_total() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let thread_id = sigil_kernel::AgentThreadId::new("agent_child_maximum_batch")?;
    focus_live_child_transcript(&mut app, &thread_id)?;

    for index in 0..sigil_kernel::MAX_PROVIDER_TURN_TOOL_CALLS {
        app.handle_worker_message(WorkerMessage::AgentThreadEvent {
            thread_id: thread_id.clone(),
            event: Box::new(RunEvent::ToolCallCompleted(ToolCall {
                id: format!("call-child-maximum-{index}"),
                name: "bash".to_owned(),
                args_json: json!({"command": format!("printf child-maximum-{index}")}).to_string(),
            })),
        })?;
    }

    let transcript = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("maximum child pending transcript");
    assert_eq!(
        transcript.total_timeline_entries,
        sigil_kernel::MAX_PROVIDER_TURN_TOOL_CALLS
    );
    assert_eq!(
        transcript.timeline_entries.len(),
        super::super::agent_flow::CHILD_AGENT_TRANSCRIPT_ENTRY_LIMIT
    );
    assert_eq!(
        app.agent_panel.child_tool_card_entry_indices.len(),
        sigil_kernel::MAX_PROVIDER_TURN_TOOL_CALLS
    );
    assert!(
        app.agent_panel
            .child_tool_card_entry_indices
            .values()
            .any(|occurrence| occurrence.entry_index().is_none()),
        "trimmed active occurrences must retain an offscreen identity"
    );

    for index in 0..sigil_kernel::MAX_PROVIDER_TURN_TOOL_CALLS {
        app.handle_worker_message(WorkerMessage::AgentThreadEvent {
            thread_id: thread_id.clone(),
            event: Box::new(RunEvent::ToolResult(production_display_bash_result(
                format!("call-child-maximum-{index}").as_str(),
                format!("child maximum completed {index}").as_str(),
            ))),
        })?;
        assert_eq!(
            app.agent_panel
                .active_child_transcript
                .as_ref()
                .expect("maximum child final transcript")
                .total_timeline_entries,
            sigil_kernel::MAX_PROVIDER_TURN_TOOL_CALLS,
            "finalizing an offscreen occurrence must not create a logical entry"
        );
    }

    let transcript = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("completed maximum child transcript");
    assert_eq!(
        transcript.timeline_entries.len(),
        super::super::agent_flow::CHILD_AGENT_TRANSCRIPT_ENTRY_LIMIT
    );
    assert!(transcript.timeline_entries.iter().all(|entry| {
        serde_json::from_str::<serde_json::Value>(&entry.text)
            .ok()
            .is_some_and(|value| {
                value.get("status").and_then(serde_json::Value::as_str) == Some("ok")
            })
    }));
    assert!(app.agent_panel.child_tool_card_entry_indices.is_empty());
    assert!(app.agent_panel.safe_child_tool_calls.is_empty());
    Ok(())
}

#[test]
fn agent_thread_event_projects_live_child_event_variants() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let thread_id = sigil_kernel::AgentThreadId::new("agent_chat_events")?;
    app.agent_panel.active_view = super::super::AgentView::Child {
        child_task_id: thread_id.as_str().to_owned(),
        child_session_ref: sigil_kernel::SessionRef::new_relative(
            "children/agent_chat_events.jsonl",
        )?,
    };

    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ReasoningDelta("think".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ReasoningDelta("\n  ".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolCallStarted(ToolCall {
            id: "call-read".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        })),
    })?;
    let pending_tool_card = &app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("started tool should initialize child transcript")
        .timeline_entries
        .last()
        .expect("pending tool card")
        .text;
    assert!(pending_tool_card.contains("read_file is running"));
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::Notice("after start".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolCallCompleted(ToolCall {
            id: "call-read".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        })),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::Notice("after complete".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolResult(ToolResult::ok(
            "call-read".to_owned(),
            "read_file".to_owned(),
            "file contents".to_owned(),
            sigil_kernel::ToolResultMeta::default(),
        ))),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::AssistantMessage(ModelMessage::assistant(
            Some("draft".to_owned()),
            Vec::new(),
        ))),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::AssistantMessage(ModelMessage::assistant(
            Some("final".to_owned()),
            Vec::new(),
        ))),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::AssistantMessage(ModelMessage::assistant(
            Some(String::new()),
            Vec::new(),
        ))),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::Notice("child notice".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::Notice("child failed hard".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::TextDelta("after notice".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolApprovalRequested {
            approval_identity: test_approval_identity("call-write"),
            effects: std::collections::BTreeSet::new(),
            analysis: sigil_kernel::ToolAnalysisStatus::Complete,
            containment: sigil_kernel::ExecutionContainmentRequest::default(),
            safe_summary: sigil_kernel::ToolPermissionSummary::default(),
            decision_reasons: Vec::new(),
            session_grant_available: false,
            session_grant_unavailable_reason: Some(sigil_kernel::ToolApprovalSessionGrantUnavailableReason {
                code: sigil_kernel::ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
            }),
            call: ToolCall {
                id: "call-write".to_owned(),
                name: "write_file".to_owned(),
                args_json: "{}".to_owned(),
            },
            spec: ToolSpec {
                name: "write_file".to_owned(),
                description: "Write".to_owned(),
                input_schema: json!({"type":"object"}),
                category: ToolCategory::File,
                access: ToolAccess::Write,
                network_effect: None,
                preview: ToolPreviewCapability::Required,
            },
            subjects: Vec::new(),
            network_effect: None,
            local_policy_decision: sigil_kernel::ApprovalMode::Ask,
            network_policy_decision: sigil_kernel::ApprovalMode::Allow,
            source_policy_decision: sigil_kernel::ApprovalMode::Allow,
            operation: sigil_kernel::ToolOperation::OverwriteFile,
            risk: sigil_kernel::PermissionRisk::Medium,
            subject_zones: Vec::new(),
            confirmation: None,
            snapshot_required: false,
            command_permission_matches: Vec::new(),
            preview: None,
        }),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::TextDelta(" after approval".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolApprovalResolved {
            call_id: "call-write".to_owned(),
            approval_request_id: "approval-call-write".to_owned(),
            approved: false,
            reason: Some("scope".to_owned()),
        }),
    })?;

    let transcript = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("live event should initialize child transcript");
    assert!(transcript.load_error.is_none());
    let entries = transcript
        .timeline_entries
        .iter()
        .map(|entry| (entry.role, entry.text.as_str()))
        .collect::<Vec<_>>();
    assert!(
        entries
            .iter()
            .any(|(role, text)| *role == TimelineRole::Thinking && text.starts_with("think"))
    );
    assert!(
        !entries
            .iter()
            .any(|(role, text)| *role == TimelineRole::Thinking && text.trim().is_empty())
    );
    assert!(!entries.contains(&(TimelineRole::Tool, "Started read_file")));
    assert!(!entries.contains(&(TimelineRole::Tool, "Completed read_file")));
    assert!(entries.iter().any(|(role, text)| {
        *role == TimelineRole::Tool
            && text.contains("\"call_id\":\"call-read\"")
            && text.contains("file contents")
    }));
    assert!(
        entries
            .iter()
            .any(|(role, text)| *role == TimelineRole::Assistant && text.starts_with("final"))
    );
    assert!(!entries.contains(&(TimelineRole::Assistant, "draft")));
    assert!(!entries.contains(&(TimelineRole::Notice, "after start")));
    assert!(!entries.contains(&(TimelineRole::Notice, "after complete")));
    assert!(!entries.contains(&(TimelineRole::Notice, "child notice")));
    assert!(entries.contains(&(TimelineRole::Notice, "child failed hard")));
    assert!(entries.contains(&(TimelineRole::Notice, "Approve write_file in child agent")));
    assert!(entries.contains(&(TimelineRole::Notice, "Approval denied for call-write")));
    let entry_count = transcript.timeline_entries.len();

    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolCallArgsDelta {
            id: "call-read".to_owned(),
            delta: "{}".to_owned(),
        }),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::Usage(UsageStats::default())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ContinuationState(
            sigil_kernel::ProviderContinuationState {
                provider_name: "deepseek".to_owned(),
                state_kind: "reasoning".to_owned(),
                message_id: None,
                opaque_blob: json!({}),
            },
        )),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id,
        event: Box::new(RunEvent::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-pro".to_owned(),
            resolved_model_route: None,
        })),
    })?;

    assert_eq!(
        app.agent_panel
            .active_child_transcript
            .as_ref()
            .expect("transcript should remain loaded")
            .timeline_entries
            .len(),
        entry_count
    );
    Ok(())
}

#[test]
fn worker_queue_status_summarizes_long_prompt() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let long_prompt = "please inspect ".repeat(8);

    app.handle_worker_message(WorkerMessage::ConversationQueueUpdated {
        items: vec![sigil_kernel::ConversationQueueItemProjection {
            queued: sigil_kernel::ConversationInputQueuedEntry {
                queue_id: sigil_kernel::ConversationInputQueueId::new("queue_long")?,
                target: sigil_kernel::ConversationInputTarget::MainThread,
                kind: sigil_kernel::ConversationInputKind::Chat,
                prompt_hash: "sha256:long".to_owned(),
                prompt: long_prompt,
                reasoning_effort: None,
                created_at_ms: None,
            },
            status: sigil_kernel::ConversationInputStatus::Queued,
            reason: None,
        }],
        paused: false,
        entries: Vec::new(),
    })?;

    let notice = app.last_notice().expect("queue notice should be set");
    assert!(notice.starts_with("pending 1 follow-up · next please inspect"));
    assert!(notice.ends_with("..."));
    Ok(())
}

#[test]
fn model_spawned_agent_events_keep_live_phase_on_agent_wait() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let spawn_call = ToolCall {
        id: "call-spawn".to_owned(),
        name: "spawn_agent".to_owned(),
        args_json: json!({
            "profile_id": "explore",
            "objective": "inspect kernel",
            "prompt": "inspect crates/sigil-kernel"
        })
        .to_string(),
    };

    app.handle(RunEvent::ToolCallCompleted(spawn_call.clone()))?;
    assert_eq!(app.run_phase(), RunPhase::Agent("explore".to_owned()));
    assert_eq!(app.last_notice(), Some("waiting for agent @explore"));

    app.handle(RunEvent::ToolApprovalRequested {
        approval_identity: test_approval_identity("call-spawn"),
        effects: std::collections::BTreeSet::new(),
        analysis: sigil_kernel::ToolAnalysisStatus::Complete,
        containment: sigil_kernel::ExecutionContainmentRequest::default(),
        safe_summary: sigil_kernel::ToolPermissionSummary::default(),
        decision_reasons: Vec::new(),
        session_grant_available: false,
        session_grant_unavailable_reason: Some(sigil_kernel::ToolApprovalSessionGrantUnavailableReason {
            code: sigil_kernel::ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
        }),
        call: spawn_call.clone(),
        spec: ToolSpec {
            name: "spawn_agent".to_owned(),
            description: "Spawn an agent".to_owned(),
            input_schema: json!({"type": "object"}),
            category: ToolCategory::Agent,
            access: ToolAccess::Execute,
            network_effect: None,
            preview: ToolPreviewCapability::Required,
        },
        subjects: Vec::new(),
        network_effect: None,
        local_policy_decision: sigil_kernel::ApprovalMode::Ask,
        network_policy_decision: sigil_kernel::ApprovalMode::Allow,
        source_policy_decision: sigil_kernel::ApprovalMode::Allow,
        operation: sigil_kernel::ToolOperation::SpawnAgent,
        risk: sigil_kernel::PermissionRisk::High,
        subject_zones: Vec::new(),
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
        preview: None,
    })?;
    assert_eq!(app.run_phase(), RunPhase::Tool("spawn_agent".to_owned()));

    app.handle(RunEvent::ToolApprovalResolved {
        call_id: "call-spawn".to_owned(),
        approval_request_id: "approval-call-spawn".to_owned(),
        approved: true,
        reason: None,
    })?;
    assert_eq!(app.run_phase(), RunPhase::Agent("explore".to_owned()));
    assert_eq!(app.last_notice(), Some("waiting for agent @explore"));

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-spawn".to_owned(),
        "spawn_agent".to_owned(),
        "{}".to_owned(),
        sigil_kernel::ToolResultMeta::default(),
    )))?;
    assert_eq!(app.run_phase(), RunPhase::Thinking);
    Ok(())
}

#[test]
fn chat_agent_thread_start_control_pushes_agent_card_with_background_hint() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let snapshot_id = sigil_kernel::AgentProfileSnapshotId::new("snapshot_explore_1")?;
    let profile_id = sigil_kernel::AgentProfileId::new("explore")?;
    let thread_id = sigil_kernel::AgentThreadId::new("agent_chat_1")?;

    app.handle(RunEvent::Control(ControlEntry::AgentProfileCaptured(
        sigil_kernel::AgentProfileCapturedEntry {
            snapshot: sigil_kernel::AgentProfileSnapshot {
                snapshot_id: snapshot_id.clone(),
                profile_id: profile_id.clone(),
                source: sigil_kernel::AgentProfileSource::System,
                source_hash: "sha256:source".to_owned(),
                profile_hash: "sha256:profile".to_owned(),
                resolved_tool_scope_hash: "tools".to_owned(),
                resolved_permission_policy_hash: "permissions".to_owned(),
                resolved_mcp_scope_hash: "mcp".to_owned(),
                resolved_skill_hashes: Vec::new(),
                trust_state: sigil_kernel::AgentTrustState::Trusted,
            },
        },
    )))?;

    app.handle(RunEvent::Control(ControlEntry::AgentThreadStarted(
        sigil_kernel::AgentThreadStartedEntry {
            thread_id: thread_id.clone(),
            parent_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
            batch_id: None,
            batch_member_key: None,
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            thread_session_ref: sigil_kernel::SessionRef::new_relative(
                "children/agents/agent_chat_1.jsonl",
            )?,
            profile_id: profile_id.clone(),
            profile_snapshot_id: snapshot_id.clone(),
            run_context: sigil_kernel::AgentRunContextSnapshot {
                profile_snapshot_id: snapshot_id,
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-pro".to_owned(),
                model_ref: None,
                reasoning_effort: None,
                workspace_root: sigil_kernel::WorkspaceRootSnapshot::new(".")?,
                effective_tool_scope_hash: "tools".to_owned(),
                effective_permission_policy_hash: "permissions".to_owned(),
                effective_mcp_scope_hash: "mcp".to_owned(),
                provider_capability_hash: "provider".to_owned(),
                model_visible_agent_index_hash: Some("agent-index".to_owned()),
                budget_policy_hash: "budget".to_owned(),
                provider_background_handle_ref: None,
            },
            objective: "inspect kernel".to_owned(),
            prompt_hash: "sha256:prompt".to_owned(),
            invocation_mode: sigil_kernel::AgentInvocationMode::JoinBeforeFinal,
            invocation_source: sigil_kernel::AgentInvocationSource::Chat,
            display_name: Some("kernel-explorer".to_owned()),
            created_at_ms: Some(42),
        },
    )))?;

    assert_eq!(app.run_phase(), RunPhase::Agent("explore".to_owned()));
    assert_eq!(app.last_notice(), Some("waiting for agent @explore"));
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Tool
            && entry.text.contains("\"tool_name\":\"spawn_agent\"")
            && entry.text.contains("\"thread_id\":\"agent_chat_1\"")
            && entry.text.contains("\"action_hint\":\"Ctrl-B background\"")
    }));

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-spawn-agent-metadata".to_owned(),
        "spawn_agent".to_owned(),
        "agent thread agent_chat_1 is running".to_owned(),
        sigil_kernel::ToolResultMeta {
            details: serde_json::json!({
                "thread_id": "agent_chat_1",
                "display_name": "kernel-explorer",
                "status": "running",
                "reason": "agent tool spawned child session",
                "retry_after_ms": 5000,
                "next_action": "continue only non-overlapping parent work"
            }),
            ..sigil_kernel::ToolResultMeta::default()
        },
    )))?;
    let agent_cards = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Tool && entry.text.contains("agent_chat_1"))
        .collect::<Vec<_>>();
    assert_eq!(agent_cards.len(), 1);
    assert!(agent_cards[0].text.contains("call-spawn-agent-metadata"));
    assert!(
        agent_cards[0]
            .text
            .contains("\"reason\":\"agent tool spawned child session\"")
    );

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-spawn-agent".to_owned(),
        "spawn_agent".to_owned(),
        serde_json::json!({
            "thread_id": "agent_chat_1",
            "display_name": "kernel-explorer",
            "status": "running",
            "terminal": false,
            "result_available": false,
            "coalescing_key": "wait_agent:agent_chat_1",
            "retry_after_ms": 5000,
            "next_action": "continue only non-overlapping parent work"
        })
        .to_string(),
        sigil_kernel::ToolResultMeta::default(),
    )))?;
    let agent_cards = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Tool && entry.text.contains("agent_chat_1"))
        .collect::<Vec<_>>();
    assert_eq!(agent_cards.len(), 1);
    assert!(agent_cards[0].text.contains("call-spawn-agent"));

    app.handle(RunEvent::Control(ControlEntry::AgentThreadStatusChanged(
        sigil_kernel::AgentThreadStatusChangedEntry {
            thread_id: thread_id.clone(),
            status: sigil_kernel::AgentThreadStatus::Running,
            reason: Some("agent moved to background".to_owned()),
            updated_at_ms: Some(43),
        },
    )))?;
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Tool
            && entry.text.contains("\"tool_name\":\"wait_agent\"")
            && entry.text.contains("\"thread_id\":\"agent_chat_1\"")
            && entry
                .text
                .contains("\"reason\":\"agent moved to background\"")
    }));
    let agent_cards = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Tool && entry.text.contains("agent_chat_1"))
        .collect::<Vec<_>>();
    assert_eq!(agent_cards.len(), 1);
    assert!(app.events.iter().any(|event| {
        event.label == "agent:status"
            && event.detail.contains(thread_id.as_str())
            && event.detail.contains("Running")
    }));
    app.handle_worker_message(WorkerMessage::AgentThreadStatusLive {
        entry: sigil_kernel::AgentThreadStatusChangedEntry {
            thread_id: thread_id.clone(),
            status: sigil_kernel::AgentThreadStatus::Completed,
            reason: Some("background finished".to_owned()),
            updated_at_ms: Some(44),
        },
    })?;
    assert!(app.session_browser.current_entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(status))
                if status.thread_id == thread_id
                    && status.status == sigil_kernel::AgentThreadStatus::Completed
        )
    }));
    let rows = app.agent_sidebar_rows();
    assert!(
        rows.iter().any(|row| {
            row.label.contains("kernel")
                && row.detail.contains("completed")
                && row.detail.contains("explore")
                && row.detail.contains("chat")
        }),
        "expected completed explore chat row, got {rows:?}"
    );

    app.handle(RunEvent::Control(ControlEntry::AgentThreadStarted(
        sigil_kernel::AgentThreadStartedEntry {
            thread_id: sigil_kernel::AgentThreadId::new("agent_task_1")?,
            parent_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
            batch_id: None,
            batch_member_key: None,
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            thread_session_ref: sigil_kernel::SessionRef::new_relative(
                "children/agents/agent_task_1.jsonl",
            )?,
            profile_id,
            profile_snapshot_id: sigil_kernel::AgentProfileSnapshotId::new("snapshot_task_1")?,
            run_context: sigil_kernel::AgentRunContextSnapshot {
                profile_snapshot_id: sigil_kernel::AgentProfileSnapshotId::new("snapshot_task_1")?,
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-pro".to_owned(),
                model_ref: None,
                reasoning_effort: None,
                workspace_root: sigil_kernel::WorkspaceRootSnapshot::new(".")?,
                effective_tool_scope_hash: "tools".to_owned(),
                effective_permission_policy_hash: "permissions".to_owned(),
                effective_mcp_scope_hash: "mcp".to_owned(),
                provider_capability_hash: "provider".to_owned(),
                model_visible_agent_index_hash: Some("agent-index".to_owned()),
                budget_policy_hash: "budget".to_owned(),
                provider_background_handle_ref: None,
            },
            objective: "task child".to_owned(),
            prompt_hash: "sha256:task-prompt".to_owned(),
            invocation_mode: sigil_kernel::AgentInvocationMode::Background,
            invocation_source: sigil_kernel::AgentInvocationSource::Task,
            display_name: None,
            created_at_ms: Some(44),
        },
    )))?;
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "control" && event.detail.contains("agent_task_1"))
    );
    Ok(())
}

#[test]
fn repeated_pending_wait_agent_results_replace_previous_tool_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    let first = ToolResult::ok(
        "call-wait-1".to_owned(),
        "wait_agent".to_owned(),
        serde_json::json!({
            "thread_id": "agent_chat_1",
            "status": "running",
            "terminal": false,
            "result_available": false,
            "retry_after_ms": 5000,
            "coalescing_key": "wait_agent:agent_chat_1",
            "next_action": "continue only non-overlapping parent work"
        })
        .to_string(),
        sigil_kernel::ToolResultMeta {
            details: serde_json::json!({
                "thread_id": "agent_chat_1",
                "status": "running",
                "retry_after_ms": 5000,
                "coalescing_key": "wait_agent:agent_chat_1"
            }),
            ..sigil_kernel::ToolResultMeta::default()
        },
    );
    let second = ToolResult::ok(
        "call-wait-2".to_owned(),
        "wait_agent".to_owned(),
        serde_json::json!({
            "thread_id": "agent_chat_1",
            "status": "running",
            "terminal": false,
            "result_available": false,
            "retry_after_ms": 4200,
            "coalesced": true,
            "polling_throttled": true,
            "coalescing_key": "wait_agent:agent_chat_1",
            "next_action": "wait_agent was called too soon; continue only non-overlapping parent work"
        })
        .to_string(),
        sigil_kernel::ToolResultMeta {
            details: serde_json::json!({
                "thread_id": "agent_chat_1",
                "status": "running",
                "retry_after_ms": 4200,
                "coalesced": true,
                "polling_throttled": true,
                "coalescing_key": "wait_agent:agent_chat_1"
            }),
            ..sigil_kernel::ToolResultMeta::default()
        },
    );

    app.handle(RunEvent::ToolResult(first))?;
    app.handle(RunEvent::ToolResult(second))?;

    let wait_cards = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Tool && entry.text.contains("wait_agent"))
        .collect::<Vec<_>>();
    assert_eq!(wait_cards.len(), 1);
    assert!(wait_cards[0].text.contains("call-wait-2"));
    assert!(wait_cards[0].text.contains("polling_throttled"));
    assert!(
        app.events
            .iter()
            .filter(|event| event.label == "tool:result" && event.detail == "wait_agent ok")
            .count()
            >= 2
    );
    Ok(())
}

#[test]
fn duplicate_tool_card_replacement_preserves_the_main_history_anchor() -> Result<()> {
    fn pending_wait(call_id: &str, retry_after_ms: u64) -> ToolResult {
        ToolResult::ok(
            call_id.to_owned(),
            "wait_agent".to_owned(),
            serde_json::json!({
                "thread_id": "agent_anchor",
                "status": "running",
                "terminal": false,
                "result_available": false,
                "retry_after_ms": retry_after_ms,
                "coalescing_key": "wait_agent:agent_anchor"
            })
            .to_string(),
            sigil_kernel::ToolResultMeta {
                details: serde_json::json!({
                    "thread_id": "agent_anchor",
                    "status": "running",
                    "retry_after_ms": retry_after_ms,
                    "coalescing_key": "wait_agent:agent_anchor"
                }),
                ..sigil_kernel::ToolResultMeta::default()
            },
        )
    }

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 18);
    for index in 0..24 {
        app.push_timeline(TimelineRole::Notice, format!("history-anchor-{index:02}"));
    }
    app.handle(RunEvent::ToolResult(pending_wait("call-anchor-1", 5_000)))?;
    let duplicate_card = app
        .timeline
        .iter()
        .rev()
        .find(|entry| entry.role == TimelineRole::Tool)
        .expect("pending wait card")
        .text
        .clone();
    for index in 24..36 {
        app.push_timeline(TimelineRole::Notice, format!("history-anchor-{index:02}"));
    }
    app.push_timeline(TimelineRole::Tool, duplicate_card);
    let duplicate_index = app.timeline.len().saturating_sub(1);
    for index in 36..72 {
        app.push_timeline(TimelineRole::Notice, format!("history-anchor-{index:02}"));
    }

    let mut before = None;
    for scroll_back in 1..=app.max_timeline_scroll_back() {
        app.timeline_scroll_back = scroll_back;
        let anchor_is_after_duplicate = matches!(
            app.capture_timeline_history_anchor(),
            Some(super::super::timeline_flow::TimelineHistoryAnchor::Main {
                entry_index,
                ..
            }) if entry_index > duplicate_index
        );
        if !anchor_is_after_duplicate {
            continue;
        }
        before = app
            .transcript_lines(app.timeline_viewport_rows())
            .into_iter()
            .flat_map(|line| line.spans.into_iter())
            .map(|span| span.content.into_owned())
            .find(|text| text.contains("history-anchor-"));
        if before.is_some() {
            break;
        }
    }
    let before = before.expect("a post-duplicate history anchor should be visible");

    app.handle(RunEvent::ToolResult(pending_wait("call-anchor-2", 4_000)))?;
    let after = app
        .transcript_lines(app.timeline_viewport_rows())
        .into_iter()
        .flat_map(|line| line.spans.into_iter())
        .map(|span| span.content.into_owned())
        .find(|text| text.contains("history-anchor-"))
        .expect("the anchored history row should remain visible");
    assert_eq!(after, before);
    Ok(())
}

#[test]
fn interleaved_pending_wait_agent_results_replace_matching_thread_card() -> Result<()> {
    fn pending_wait(call_id: &str, thread_id: &str, retry_after_ms: u64) -> ToolResult {
        let key = format!("wait_agent:{thread_id}");
        ToolResult::ok(
            call_id.to_owned(),
            "wait_agent".to_owned(),
            serde_json::json!({
                "thread_id": thread_id,
                "status": "running",
                "terminal": false,
                "result_available": false,
                "retry_after_ms": retry_after_ms,
                "coalescing_key": key,
                "next_action": "continue only non-overlapping parent work"
            })
            .to_string(),
            sigil_kernel::ToolResultMeta {
                details: serde_json::json!({
                    "thread_id": thread_id,
                    "status": "running",
                    "retry_after_ms": retry_after_ms,
                    "coalescing_key": key
                }),
                ..sigil_kernel::ToolResultMeta::default()
            },
        )
    }

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ToolResult(pending_wait(
        "call-wait-a-1",
        "agent_chat_a",
        5000,
    )))?;
    app.handle(RunEvent::ToolResult(pending_wait(
        "call-wait-b-1",
        "agent_chat_b",
        5000,
    )))?;
    app.handle(RunEvent::ToolResult(pending_wait(
        "call-wait-a-2",
        "agent_chat_a",
        4200,
    )))?;

    let wait_cards = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Tool && entry.text.contains("wait_agent"))
        .collect::<Vec<_>>();
    assert_eq!(wait_cards.len(), 2);
    assert!(wait_cards.iter().any(|entry| {
        entry.text.contains("call-wait-a-2") && entry.text.contains("agent_chat_a")
    }));
    assert!(wait_cards.iter().any(|entry| {
        entry.text.contains("call-wait-b-1") && entry.text.contains("agent_chat_b")
    }));
    assert!(
        !wait_cards
            .iter()
            .any(|entry| entry.text.contains("call-wait-a-1"))
    );
    Ok(())
}

#[test]
fn ctrl_b_during_agent_wait_requests_background() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_worker_message(WorkerMessage::AgentRunStarted {
        profile_id: "explore".to_owned(),
        prompt: "inspect kernel".to_owned(),
    })?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL))?;

    assert!(matches!(action, Some(AppAction::BackgroundActiveAgent)));
    assert_eq!(app.last_notice(), Some("agent background requested"));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "agent" && event.detail == "background requested")
    );
    Ok(())
}

#[test]
fn worker_queue_messages_update_live_rows_and_dispatch_user_prompt() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let queue_id = sigil_kernel::ConversationInputQueueId::new("queue_1").expect("valid queue id");
    let queued = sigil_kernel::ConversationInputQueuedEntry {
        queue_id: queue_id.clone(),
        target: sigil_kernel::ConversationInputTarget::MainThread,
        kind: sigil_kernel::ConversationInputKind::Chat,
        prompt_hash: "sha256:queue".to_owned(),
        prompt: "follow up after current run".to_owned(),
        reasoning_effort: Some(ReasoningEffort::Max),
        created_at_ms: Some(1),
    };
    let entry = SessionLogEntry::Control(ControlEntry::ConversationInputQueued(queued.clone()));

    app.handle_worker_message(WorkerMessage::ConversationQueueUpdated {
        items: vec![sigil_kernel::ConversationQueueItemProjection {
            queued,
            status: sigil_kernel::ConversationInputStatus::Queued,
            reason: None,
        }],
        paused: false,
        entries: vec![entry],
    })?;

    assert_eq!(
        app.last_notice(),
        Some("pending 1 follow-up · next follow up after current run")
    );
    assert_eq!(app.composer_queue_rows().len(), 1);
    assert!(app.events.iter().any(|event| {
        event.label == "follow-up:update"
            && event.detail.contains("next follow up after current run")
    }));

    app.handle_worker_message(WorkerMessage::ConversationQueueDispatchStarted {
        queue_id: queue_id.clone(),
        prompt: "follow up after current run".to_owned(),
    })?;
    assert!(app.runtime.is_busy);
    assert_eq!(app.run_phase(), RunPhase::Thinking);
    assert_eq!(app.last_notice(), Some("running follow-up"));
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::User && entry.text == "follow up after current run"
    }));
    assert!(app.events.iter().any(|event| {
        event.label == "follow-up:dispatch" && event.detail.contains(queue_id.as_str())
    }));

    let mut dispatching = sigil_kernel::ConversationQueueItemProjection {
        queued: sigil_kernel::ConversationInputQueuedEntry {
            queue_id: queue_id.clone(),
            target: sigil_kernel::ConversationInputTarget::MainThread,
            kind: sigil_kernel::ConversationInputKind::Chat,
            prompt_hash: "sha256:queue".to_owned(),
            prompt: "follow up after current run".to_owned(),
            reasoning_effort: Some(ReasoningEffort::Max),
            created_at_ms: Some(1),
        },
        status: sigil_kernel::ConversationInputStatus::Dispatching,
        reason: Some("promotion_bound".to_owned()),
    };
    app.handle_worker_message(WorkerMessage::ConversationQueueUpdated {
        items: vec![dispatching.clone()],
        paused: false,
        entries: vec![
            SessionLogEntry::Control(ControlEntry::ConversationInputQueued(
                dispatching.queued.clone(),
            )),
            SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(
                sigil_kernel::ConversationInputStatusEntry {
                    queue_id: queue_id.clone(),
                    status: sigil_kernel::ConversationInputStatus::Dispatching,
                    reason: dispatching.reason.take(),
                    updated_at_ms: Some(2),
                },
            )),
        ],
    })?;
    assert!(app.composer_queue_rows().is_empty());
    assert_eq!(app.last_notice(), Some("no follow-ups pending"));

    app.handle_worker_message(WorkerMessage::ConversationQueueUpdated {
        items: Vec::new(),
        paused: true,
        entries: Vec::new(),
    })?;
    assert_eq!(app.last_notice(), Some("no follow-ups pending"));
    Ok(())
}

#[test]
fn worker_command_conversion_covers_remaining_variants_and_panics_for_config_updates() {
    let app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    assert!(matches!(
        app.into_worker_command(AppAction::SubmitPrompt("draft".to_owned())),
        WorkerCommand::SubmitPrompt { prompt, .. } if prompt == "draft"
    ));
    let attachment = sigil_kernel::ImageAttachment::from_bytes(
        "image_1",
        sigil_kernel::ImageMimeType::Png,
        1,
        1,
        vec![1],
    )
    .expect("valid attachment metadata");
    assert!(matches!(
        app.into_worker_command(AppAction::SubmitPromptWithAttachments {
            prompt: "inspect".to_owned(),
            attachments: vec![attachment],
        }),
        WorkerCommand::SubmitPromptWithAttachments {
            prompt,
            attachments,
            ..
        } if prompt == "inspect" && attachments.len() == 1
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::QueueConversationInput {
            prompt: "queued draft".to_owned(),
            kind: sigil_kernel::ConversationInputKind::Chat,
            target: sigil_kernel::ConversationInputTarget::MainThread,
        }),
        WorkerCommand::QueueConversationInput {
            prompt,
            kind: sigil_kernel::ConversationInputKind::Chat,
            target: sigil_kernel::ConversationInputTarget::MainThread,
            ..
        } if prompt == "queued draft"
    ));
    let queue_id = sigil_kernel::ConversationInputQueueId::new("queue_1").expect("valid queue id");
    assert!(matches!(
        app.into_worker_command(AppAction::CancelQueuedConversationInput {
            queue_id: queue_id.clone(),
        }),
        WorkerCommand::CancelQueuedConversationInput { queue_id }
            if queue_id.as_str() == "queue_1"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::EditQueuedConversationInput {
            queue_id: queue_id.clone(),
            prompt: "edited draft".to_owned(),
        }),
        WorkerCommand::EditQueuedConversationInput { queue_id, prompt, .. }
            if queue_id.as_str() == "queue_1" && prompt == "edited draft"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::MoveQueuedConversationInput {
            queue_id: queue_id.clone(),
            direction: crate::runner::QueueMoveDirection::Up,
        }),
        WorkerCommand::MoveQueuedConversationInput {
            queue_id,
            direction: crate::runner::QueueMoveDirection::Up,
        } if queue_id.as_str() == "queue_1"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::PromoteQueuedConversationInput {
            queue_id: queue_id.clone(),
        }),
        WorkerCommand::PromoteQueuedConversationInput { queue_id }
            if queue_id.as_str() == "queue_1"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::SendQueuedConversationInputNow {
            queue_id: queue_id.clone(),
        }),
        WorkerCommand::SendQueuedConversationInputNow { queue_id }
            if queue_id.as_str() == "queue_1"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::SetConversationQueuePaused { paused: true }),
        WorkerCommand::SetConversationQueuePaused { paused: true }
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::InvokeInlineSkill {
            skill_id: "repo-review".to_owned(),
            arguments: "crates".to_owned(),
        }),
        WorkerCommand::InvokeInlineSkill {
            skill_id,
            arguments,
            ..
        } if skill_id == "repo-review" && arguments == "crates"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::InvokeChildSessionSkill {
            skill_id: "repo-audit".to_owned(),
            arguments: "--depth full".to_owned(),
        }),
        WorkerCommand::InvokeChildSessionSkill {
            skill_id,
            arguments,
        } if skill_id == "repo-audit" && arguments == "--depth full"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::InvokeAgentProfile {
            profile_id: "repo-review".to_owned(),
            prompt: "audit crates".to_owned(),
            parent_prompt: "@repo-review audit crates".to_owned(),
        }),
        WorkerCommand::InvokeAgentProfile { profile_id, prompt, parent_prompt }
            if profile_id == "repo-review"
                && prompt == "audit crates"
                && parent_prompt == "@repo-review audit crates"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::ApprovalDecision {
            call_id: "call-1".to_owned(),
            approval_request_id: "approval-1".to_owned(),
            approved: true,
        }),
        WorkerCommand::ApprovalCommand(command)
            if command.client_id == "sigil-tui"
                && matches!(
                    command.payload,
                    crate::runner::WorkerApprovalCommand::Decision {
                        ref call_id,
                        ref approval_request_id,
                        approved,
                    } if call_id == "call-1"
                        && approval_request_id == "approval-1"
                        && approved
                )
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::ApprovalDecisionWithArgs {
            call_id: "call-spawn".to_owned(),
            approval_request_id: "approval-spawn".to_owned(),
            args_json: r#"{"mode":"background"}"#.to_owned(),
        }),
        WorkerCommand::ApprovalCommand(command)
            if command.client_id == "sigil-tui"
                && matches!(
                    command.payload,
                    crate::runner::WorkerApprovalCommand::DecisionWithArgs {
                        ref call_id,
                        ref approval_request_id,
                        ref args_json,
                    } if call_id == "call-spawn"
                        && approval_request_id == "approval-spawn"
                        && args_json.contains("background")
                )
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::BackgroundActiveAgent),
        WorkerCommand::BackgroundActiveAgent
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::CancelRun),
        WorkerCommand::CancelRun
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::CancelTerminalTask {
            identity: TerminalTaskControlIdentity {
                session_scope_id: "session-scope-1".to_owned(),
                run_id: "foreground-run-1".to_owned(),
                task_id: "terminal-1".to_owned(),
                expected_generation: 7,
            },
        }),
        WorkerCommand::CancelTerminalTask { identity }
            if identity.task_id == "terminal-1"
                && identity.run_id == "foreground-run-1"
                && identity.expected_generation == 7
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::CloseAgent {
            thread_id: sigil_kernel::AgentThreadId::new("thread-1")
                .expect("test thread id should be valid"),
            reason: Some("done".to_owned()),
        }),
        WorkerCommand::CloseAgent {
            thread_id,
            reason: Some(reason),
        } if thread_id.as_str() == "thread-1" && reason == "done"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::CancelAgent {
            thread_id: sigil_kernel::AgentThreadId::new("thread-1")
                .expect("test thread id should be valid"),
            reason: Some("stop".to_owned()),
        }),
        WorkerCommand::CancelAgent {
            thread_id,
            reason: Some(reason),
        } if thread_id.as_str() == "thread-1" && reason == "stop"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::MessageAgent {
            thread_id: sigil_kernel::AgentThreadId::new("thread-1")
                .expect("test thread id should be valid"),
            prompt: "keep going".to_owned(),
        }),
        WorkerCommand::MessageAgent { thread_id, prompt }
            if thread_id.as_str() == "thread-1" && prompt == "keep going"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::StartV2Compaction),
        WorkerCommand::StartV2Compaction
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::PreviewV2Compaction),
        WorkerCommand::PreviewV2Compaction
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::ApplyV2Compaction { request_id: 7 }),
        WorkerCommand::ApplyV2Compaction { request_id: 7 }
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::CancelV2CompactionReview { request_id: 7 }),
        WorkerCommand::CancelV2CompactionReview { request_id: 7 }
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::CheckChangedFilesDiagnostics),
        WorkerCommand::CheckChangedFilesDiagnostics
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::CleanMutationArtifacts {
            target: sigil_kernel::MutationArtifactCleanupTarget::Recommended,
        }),
        WorkerCommand::CleanMutationArtifacts {
            target: sigil_kernel::MutationArtifactCleanupTarget::Recommended,
        }
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::DeleteMutationArtifact {
            artifact_id: "mutation-artifact:sha256:def".to_owned(),
        }),
        WorkerCommand::DeleteMutationArtifact { ref artifact_id }
            if artifact_id == "mutation-artifact:sha256:def"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::ApproveVerificationCheck {
            check_spec_id: "cargo-test".to_owned(),
        }),
        WorkerCommand::ApproveVerificationCheck { ref check_spec_id }
            if check_spec_id == "cargo-test"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::SandboxVerificationCheck {
            check_spec_id: "cargo-test".to_owned(),
        }),
        WorkerCommand::SandboxVerificationCheck { ref check_spec_id }
            if check_spec_id == "cargo-test"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::StartNewSession {
            session_log_path: std::path::PathBuf::from("session-new.jsonl"),
        }),
        WorkerCommand::StartNewSession { session_log_path }
            if session_log_path == std::path::Path::new("session-new.jsonl")
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::SwitchSession {
            session_log_path: std::path::PathBuf::from("session.jsonl"),
        }),
        WorkerCommand::SwitchSession { session_log_path, .. }
            if session_log_path == std::path::Path::new("session.jsonl")
    ));
    assert!(matches!(
        AppState::shutdown_command(),
        WorkerCommand::Shutdown
    ));

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        app.into_worker_command(AppAction::ConfigSaved {
            root_config: Box::new(test_config()),
        })
    }));
    assert!(panic.is_err());
}

#[test]
fn attachment_retry_binding_is_scoped_to_the_exact_resume_target() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let busy_target = std::path::PathBuf::from("session-busy.jsonl");
    let other_target = std::path::PathBuf::from("session-other.jsonl");
    app.mark_pending_session_transition_target(busy_target.clone());
    app.set_pending_session_route_recovery(
        sigil_kernel::PublicRouteRecoveryCode::SessionAlreadyActive,
        "busy-generation-binding".to_owned(),
    );

    assert!(matches!(
        app.into_worker_command(AppAction::SwitchSession {
            session_log_path: other_target.clone(),
        }),
        WorkerCommand::SwitchSession {
            session_log_path,
            attachment_recovery_binding: None,
        } if session_log_path == other_target
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::SwitchSession {
            session_log_path: busy_target.clone(),
        }),
        WorkerCommand::SwitchSession {
            session_log_path,
            attachment_recovery_binding: Some(binding),
        } if session_log_path == busy_target && binding == "busy-generation-binding"
    ));
}

#[test]
fn terminal_task_updated_syncs_session_and_pushes_tool_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.pending_terminal_cancel_confirmation = Some("terminal-1".to_owned());
    let running = worker_terminal_entry("terminal-1", sigil_kernel::TerminalTaskStatus::Running)?;
    let running_entries = vec![SessionLogEntry::Control(ControlEntry::TerminalTask(
        running.clone(),
    ))];
    app.handle_worker_message(WorkerMessage::TerminalTaskUpdated {
        identity: TerminalTaskControlIdentity {
            session_scope_id: "session-scope-1".to_owned(),
            run_id: "foreground-run-1".to_owned(),
            task_id: "terminal-1".to_owned(),
            expected_generation: 1,
        },
        entry: running,
        entries: running_entries,
    })?;

    let entry = worker_terminal_entry("terminal-1", sigil_kernel::TerminalTaskStatus::Cancelled)?;
    let entries = vec![SessionLogEntry::Control(ControlEntry::TerminalTask(
        entry.clone(),
    ))];

    app.handle_worker_message(WorkerMessage::TerminalTaskUpdated {
        identity: TerminalTaskControlIdentity {
            session_scope_id: "session-scope-1".to_owned(),
            run_id: "foreground-run-1".to_owned(),
            task_id: "terminal-1".to_owned(),
            expected_generation: 1,
        },
        entry,
        entries,
    })?;

    assert!(app.pending_terminal_cancel_confirmation.is_none());
    assert_eq!(
        app.last_notice(),
        Some("terminal task terminal-1 cancelled")
    );
    assert!(app.task_sidebar_lines().is_empty());
    let tool_entries = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Tool && entry.text.contains("terminal_task"))
        .collect::<Vec<_>>();
    assert_eq!(tool_entries.len(), 1);
    let tool_entry = tool_entries
        .first()
        .expect("expected terminal task card after replacement");
    let payload: serde_json::Value = serde_json::from_str(&tool_entry.text)?;
    assert_eq!(payload["tool_name"], "terminal_task");
    assert_eq!(
        payload["metadata"]["details"]["terminal_task"]["status"],
        "cancelled"
    );
    assert!(app.events.iter().any(|event| {
        event.label == "terminal" && event.detail == "terminal-1 status=cancelled"
    }));
    Ok(())
}

#[test]
fn terminal_tool_results_replace_existing_task_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-terminal-start",
        "terminal_start",
        "started terminal task terminal-1",
        ToolResultMeta {
            details: json!({
                "task_id": "terminal-1",
                "status": "running",
                "command": "./scripts/check-touched.sh --tier quick",
                "cwd": ".",
                "shell": "sh",
                "log_path": ".sigil/tasks/terminal-1/output.log",
                "created_at_ms": 10,
                "updated_at_ms": 10,
                "output_preview": null,
                "output_hash": null,
                "output_truncated": false
            }),
            ..ToolResultMeta::default()
        },
    )))?;
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-terminal-read",
        "terminal_read",
        "read terminal task terminal-1",
        ToolResultMeta {
            details: json!({
                "task_id": "terminal-1",
                "offset": 0,
                "next_offset": 20,
                "returned_bytes": 20,
                "total_bytes": 20,
                "limit_bytes": 4096,
                "truncated": false,
                "terminal_task": {
                    "task_id": "terminal-1",
                    "status": "exited",
                    "status_detail": {"exited": {"exit_code": 0}},
                    "command": "./scripts/check-touched.sh --tier quick",
                    "cwd": ".",
                    "shell": "sh",
                    "log_path": ".sigil/tasks/terminal-1/output.log",
                    "created_at_ms": 10,
                    "updated_at_ms": 30,
                    "output_preview": "ok",
                    "output_hash": "hash",
                    "output_truncated": false
                }
            }),
            ..ToolResultMeta::default()
        },
    )))?;

    let terminal_cards = app
        .timeline
        .iter()
        .filter(|entry| {
            entry.role == TimelineRole::Tool
                && (entry.text.contains("terminal_start")
                    || entry.text.contains("terminal_read")
                    || entry.text.contains("terminal_task"))
        })
        .collect::<Vec<_>>();
    assert_eq!(terminal_cards.len(), 1);
    let payload: serde_json::Value = serde_json::from_str(&terminal_cards[0].text)?;
    assert_eq!(payload["tool_name"], "terminal_read");
    assert_eq!(
        payload["metadata"]["details"]["terminal_task"]["status"],
        "exited"
    );
    assert_eq!(
        app.timeline_state.selected_tool_activity_key.as_deref(),
        Some("terminal_task:terminal-1")
    );
    Ok(())
}

#[test]
fn terminal_progress_updates_existing_task_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    for (sequence, preview) in [(1, "phase one"), (2, "phase two")] {
        app.handle(RunEvent::ToolProgress(sigil_kernel::ToolProgressEvent {
            execution_id: sigil_kernel::ToolExecutionId::new("terminal-progress")?,
            call_id: "call-terminal-progress".to_owned(),
            tool_name: "terminal_start".to_owned(),
            sequence,
            status: "running".to_owned(),
            message: Some(format!("progress {sequence}")),
            output_preview: Some(preview.to_owned()),
            output_log_ref: Some(std::path::PathBuf::from(
                ".sigil/tasks/terminal-progress/output.log",
            )),
            total_bytes: Some(preview.len() as u64),
            updated_at_ms: Some(sequence),
            details: json!({
                "task_id": "terminal-progress",
                "status": "running",
                "status_detail": {"state": "running"},
                "command": "./scripts/check-touched.sh --tier quick",
                "cwd": ".",
                "shell": "sh",
                "log_path": ".sigil/tasks/terminal-progress/output.log",
                "created_at_ms": 1,
                "updated_at_ms": sequence,
                "output_preview": preview,
                "output_hash": null,
                "output_truncated": false,
                "execution_mode": "foreground"
            }),
        }))?;
    }

    let terminal_cards = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Tool && entry.text.contains("terminal_start"))
        .collect::<Vec<_>>();
    assert_eq!(terminal_cards.len(), 1);
    assert!(terminal_cards[0].text.contains("phase two"));
    assert!(!terminal_cards[0].text.contains("phase one"));
    assert_eq!(
        app.timeline_state.selected_tool_activity_key.as_deref(),
        Some("terminal_task:terminal-progress")
    );
    Ok(())
}

#[test]
fn tool_progress_and_result_update_existing_card_by_execution_id() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ToolProgress(sigil_kernel::ToolProgressEvent {
        execution_id: sigil_kernel::ToolExecutionId::new("execution-only-progress")?,
        call_id: "call-execution-progress".to_owned(),
        tool_name: "terminal_start".to_owned(),
        sequence: 1,
        status: "running".to_owned(),
        message: Some("progress".to_owned()),
        output_preview: Some("phase one".to_owned()),
        output_log_ref: None,
        total_bytes: Some(9),
        updated_at_ms: Some(1),
        details: json!({
            "status": "running",
            "execution_mode": "foreground"
        }),
    }))?;
    app.handle(RunEvent::ToolResult(sigil_kernel::ToolResult::ok(
        "call-execution-progress",
        "terminal_start",
        "terminal task exited · verdict passed",
        sigil_kernel::ToolResultMeta {
            exit_code: Some(0),
            details: json!({
                "execution_id": "execution-only-progress",
                "status": "exited",
                "execution_mode": "foreground",
                "verdict": "passed"
            }),
            ..sigil_kernel::ToolResultMeta::default()
        },
    )))?;

    let terminal_cards = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Tool && entry.text.contains("terminal_start"))
        .collect::<Vec<_>>();
    assert_eq!(terminal_cards.len(), 1);
    assert!(terminal_cards[0].text.contains("verdict passed"));
    assert!(!terminal_cards[0].text.contains("phase one"));
    Ok(())
}

fn production_display_bash_result(call_id: &str, content: &str) -> ToolResult {
    ToolResult::ok(
        call_id,
        "bash",
        content,
        ToolResultMeta {
            details: json!({
                "status_label": "ok",
                "summary": format!("bash ok ({} observed bytes, {} persisted bytes)", content.len(), content.len()),
                "preview": content,
                "observed_bytes": content.len(),
                "persisted_bytes": content.len(),
                "has_more": false,
                "display_capabilities": ["copy_summary"],
                "preview_truncated": false
            }),
            ..ToolResultMeta::default()
        },
    )
}

#[test]
fn bash_progress_and_production_final_merge_once_with_safe_command() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.secret_redactor = sigil_kernel::SecretRedactor::from_values(["supersecret-token"]);
    let call_id = "call-bash-production";
    app.handle(RunEvent::ToolCallCompleted(ToolCall {
        id: call_id.to_owned(),
        name: "bash".to_owned(),
        args_json: json!({
            "command": "printf supersecret-token && cargo check --workspace"
        })
        .to_string(),
    }))?;
    app.handle(RunEvent::ToolProgress(sigil_kernel::ToolProgressEvent {
        execution_id: sigil_kernel::ToolExecutionId::new("bash-execution-production")?,
        call_id: call_id.to_owned(),
        tool_name: "bash".to_owned(),
        sequence: 1,
        status: "running".to_owned(),
        message: Some("foreground shell command is running".to_owned()),
        output_preview: None,
        output_log_ref: None,
        total_bytes: Some(0),
        updated_at_ms: None,
        details: json!({"execution_mode":"foreground"}),
    }))?;
    let progress_card = app
        .timeline
        .iter()
        .find(|entry| entry.role == TimelineRole::Tool && entry.text.contains(call_id))
        .expect("tool progress should render one card");
    let progress_payload: serde_json::Value = serde_json::from_str(&progress_card.text)?;
    assert_eq!(progress_payload["status"], "running");
    let progress_timeline = app.timeline_plain_lines().join("\n");
    assert!(progress_timeline.contains("RUNNING"));
    assert!(!progress_timeline.contains("✓ OK"));

    app.handle(RunEvent::ToolResult(production_display_bash_result(
        call_id,
        "check completed",
    )))?;

    let cards = app
        .timeline
        .iter()
        .filter(|entry| {
            entry.role == TimelineRole::Tool
                && serde_json::from_str::<serde_json::Value>(&entry.text)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("call_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .is_some_and(|rendered_call_id| rendered_call_id == call_id)
        })
        .collect::<Vec<_>>();
    assert_eq!(cards.len(), 1);
    assert!(cards[0].text.contains("check completed"));
    assert!(
        !cards[0]
            .text
            .contains("foreground shell command is running")
    );
    assert!(!cards[0].text.contains("supersecret-token"));
    assert!(
        cards[0]
            .text
            .contains("command=printf [redacted] && cargo check --workspace")
    );
    assert!(cards[0].text.contains("bash-execution-production"));
    assert!(!app.safe_tool_calls.contains_key(call_id));
    assert!(!app.tool_progress_execution_ids.contains_key(call_id));
    assert!(app.tool_progress_entry_indices.is_empty());
    Ok(())
}

#[test]
fn reused_bash_call_and_execution_ids_keep_each_turns_progress_and_final_distinct() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let call_id = "provider-reused-bash-call";
    let execution_id = "bash-reused-execution";

    for (turn_index, (command, output)) in [
        ("printf first", "first final output"),
        ("printf second", "second final output"),
    ]
    .into_iter()
    .enumerate()
    {
        app.handle(RunEvent::ToolCallCompleted(ToolCall {
            id: call_id.to_owned(),
            name: "bash".to_owned(),
            args_json: json!({"command": command}).to_string(),
        }))?;
        for sequence in 1..=2 {
            app.handle(RunEvent::ToolProgress(sigil_kernel::ToolProgressEvent {
                execution_id: sigil_kernel::ToolExecutionId::new(execution_id)?,
                call_id: call_id.to_owned(),
                tool_name: "bash".to_owned(),
                sequence,
                status: "running".to_owned(),
                message: Some(format!("turn {turn_index} progress {sequence}")),
                output_preview: None,
                output_log_ref: None,
                total_bytes: Some(0),
                updated_at_ms: Some(sequence),
                details: json!({"execution_mode":"foreground"}),
            }))?;
            assert_eq!(
                app.timeline
                    .iter()
                    .filter(|entry| {
                        entry.role == TimelineRole::Tool
                            && serde_json::from_str::<serde_json::Value>(&entry.text)
                                .ok()
                                .and_then(|value| {
                                    value
                                        .get("call_id")
                                        .and_then(serde_json::Value::as_str)
                                        .map(str::to_owned)
                                })
                                .is_some_and(|rendered_call_id| rendered_call_id == call_id)
                    })
                    .count(),
                turn_index + 1,
                "multiple progress events must merge only within the active turn"
            );
        }
        app.handle(RunEvent::ToolResult(production_display_bash_result(
            call_id, output,
        )))?;
    }

    let cards = app
        .timeline
        .iter()
        .filter(|entry| {
            entry.role == TimelineRole::Tool
                && serde_json::from_str::<serde_json::Value>(&entry.text)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("call_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .is_some_and(|rendered_call_id| rendered_call_id == call_id)
        })
        .collect::<Vec<_>>();
    assert_eq!(cards.len(), 2);
    assert!(cards[0].text.contains("command=printf first"));
    assert!(cards[0].text.contains("first final output"));
    assert!(cards[1].text.contains("command=printf second"));
    assert!(cards[1].text.contains("second final output"));
    assert!(cards.iter().all(|card| card.text.contains(execution_id)));
    assert!(cards.iter().all(
        |card| !card.text.contains("turn 0 progress") && !card.text.contains("turn 1 progress")
    ));
    Ok(())
}

#[test]
fn untracked_final_with_reused_execution_id_does_not_replace_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let call_id = "provider-reused-final-call";
    let execution_id = "bash-reused-final-execution";

    for (command, output) in [
        ("printf first", "first final output"),
        ("printf second", "second final output"),
    ] {
        app.handle(RunEvent::ToolCallCompleted(ToolCall {
            id: call_id.to_owned(),
            name: "bash".to_owned(),
            args_json: json!({"command": command}).to_string(),
        }))?;
        let mut result = production_display_bash_result(call_id, output);
        result
            .metadata
            .details
            .as_object_mut()
            .expect("production display details should be an object")
            .insert(
                "execution_id".to_owned(),
                serde_json::Value::String(execution_id.to_owned()),
            );
        app.handle(RunEvent::ToolResult(result))?;
    }

    let cards = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Tool && entry.text.contains(execution_id))
        .collect::<Vec<_>>();
    assert_eq!(cards.len(), 2);
    assert!(cards[0].text.contains("command=printf first"));
    assert!(cards[0].text.contains("first final output"));
    assert!(cards[1].text.contains("command=printf second"));
    assert!(cards[1].text.contains("second final output"));
    Ok(())
}

#[test]
fn first_progress_card_in_maximum_provider_batch_merges_with_its_final() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    for index in 0..sigil_kernel::MAX_PROVIDER_TURN_TOOL_CALLS {
        app.handle(RunEvent::ToolProgress(sigil_kernel::ToolProgressEvent {
            execution_id: sigil_kernel::ToolExecutionId::new(format!("execution-{index}"))?,
            call_id: format!("call-{index}"),
            tool_name: "bash".to_owned(),
            sequence: 1,
            status: "running".to_owned(),
            message: Some(format!("progress-{index}")),
            output_preview: None,
            output_log_ref: None,
            total_bytes: Some(0),
            updated_at_ms: None,
            details: json!({"execution_mode":"foreground"}),
        }))?;
        app.push_timeline(
            TimelineRole::Tool,
            json!({
                "call_id": format!("background-{index}"),
                "tool_name": "background_status",
                "status": "ok",
                "preview_kind": "text",
                "preview_lines": [format!("background update {index}")],
                "hidden_lines": 0
            })
            .to_string(),
        );
    }
    let expected_card_count = sigil_kernel::MAX_PROVIDER_TURN_TOOL_CALLS * 2;
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Tool)
            .count(),
        expected_card_count
    );

    app.handle(RunEvent::ToolResult(production_display_bash_result(
        "call-0",
        "first final output",
    )))?;

    let tool_cards = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Tool)
        .collect::<Vec<_>>();
    assert_eq!(tool_cards.len(), expected_card_count);
    assert!(tool_cards[0].text.contains("first final output"));
    assert!(!tool_cards[0].text.contains("progress-0"));
    assert!(tool_cards[0].text.contains("execution-0"));
    assert!(!app.tool_progress_entry_indices.contains_key("execution-0"));
    Ok(())
}

#[test]
fn repeated_call_id_without_progress_keeps_distinct_final_cards() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let call_id = "provider-reused-call-id";
    for (command, output) in [("echo first", "first done"), ("echo second", "second done")] {
        app.handle(RunEvent::ToolCallCompleted(ToolCall {
            id: call_id.to_owned(),
            name: "bash".to_owned(),
            args_json: json!({"command":command}).to_string(),
        }))?;
        app.handle(RunEvent::ToolResult(production_display_bash_result(
            call_id, output,
        )))?;
    }

    let cards = app
        .timeline
        .iter()
        .filter(|entry| {
            entry.role == TimelineRole::Tool
                && serde_json::from_str::<serde_json::Value>(&entry.text)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("call_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .is_some_and(|rendered_call_id| rendered_call_id == call_id)
        })
        .collect::<Vec<_>>();
    assert_eq!(cards.len(), 2);
    assert!(cards[0].text.contains("command=echo first"));
    assert!(cards[1].text.contains("command=echo second"));
    Ok(())
}

#[test]
fn new_session_started_restores_empty_session_view() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.push_timeline(TimelineRole::Assistant, "old context");
    let new_session_log_path = std::path::PathBuf::from(".sigil/sessions/session-new.jsonl");

    app.handle_worker_message(WorkerMessage::NewSessionStarted {
        session_log_path: new_session_log_path.clone(),
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        entries: vec![SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-pro".to_owned(),
            resolved_model_route: None,
        })],
    })?;

    assert_eq!(app.session_log_path, new_session_log_path);
    assert_eq!(app.runtime.model_name, "deepseek-v4-pro");
    assert_eq!(app.last_notice(), Some("started new session"));
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "started new session")
    );
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Assistant && entry.text == "old context")
    );
    Ok(())
}

#[test]
fn v2_compaction_review_requires_admission_before_it_can_apply() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let temp = tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("preview.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("old request")))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("old response".to_owned()),
        Vec::new(),
    )))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("latest request")))?;
    let preview = adaptive_test_compaction_preview(&store)?;

    app.handle_worker_message(WorkerMessage::V2CompactionPreviewed {
        state: V2CompactionPreviewState::Review(Box::new(V2CompactionReview {
            request_id: 41,
            strategy: sigil_kernel::CompactionStrategy::CacheAwareV3,
            preview,
            admission: V2CompactionAdmission::Unavailable {
                reason: "verified tokenizer is not installed".to_owned(),
            },
            tool_output_shrink_candidates: Vec::new(),
            continuity: None,
            native_carrier_requested: false,
        })),
    })?;

    assert_eq!(app.modal_title(), Some("Context Compaction"));
    let lines = app.modal_lines().join("\n");
    assert!(lines.contains("Review"));
    assert!(lines.contains("fold: 2 message(s)"));
    assert!(lines.contains("apply: unavailable"));
    assert_eq!(
        app.last_notice(),
        Some("review V2 compaction; local target request admission is unavailable")
    );
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        action,
        Some(AppAction::CancelV2CompactionReview { request_id: 41 })
    ));
    assert!(!app.has_modal());
    assert_eq!(app.last_notice(), Some("closed V2 compaction preview"));
    Ok(())
}

#[test]
fn admitted_v2_compaction_review_confirms_an_apply_action() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let temp = tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("admitted-preview.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("old request")))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("old response".to_owned()),
        Vec::new(),
    )))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("latest request")))?;
    let preview = adaptive_test_compaction_preview(&store)?;

    app.handle_worker_message(WorkerMessage::V2CompactionPreviewed {
        state: V2CompactionPreviewState::Review(Box::new(V2CompactionReview {
            request_id: 42,
            strategy: sigil_kernel::CompactionStrategy::CacheAwareV3,
            preview,
            admission: V2CompactionAdmission::Ready {
                before_input_tokens: 200,
                input_tokens: 120,
                context_window_tokens: 1_000_000,
                output_tokens: 32_768,
                safety_buffer_tokens: 8_192,
                savings_tokens: 80,
                savings_ratio_ppm: 400_000,
                minimum_savings_tokens: 64,
                minimum_savings_ratio_ppm: 50_000,
                summary_usage_observed: true,
                deterministic_emergency_fallback: false,
                summary_cache_read_tokens: 80,
                summary_uncached_input_tokens: 20,
                summary_output_tokens: 10,
                summary_cost_nano_usd: Some(42),
                economics_v2: None,
            },
            tool_output_shrink_candidates: Vec::new(),
            continuity: None,
            native_carrier_requested: false,
        })),
    })?;

    let lines = app.modal_lines().join("\n");
    assert!(lines.contains("target request: verified locally"));
    assert!(
        lines.contains("summary call: cache-read 80 · uncached 20 · output 10 tokens · 42 nUSD")
    );
    assert!(lines.contains("Enter apply"));
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        action,
        Some(AppAction::ApplyV2Compaction { request_id: 42 })
    ));
    assert!(!app.has_modal());
    assert_eq!(app.last_notice(), Some("applying V2 compaction"));
    Ok(())
}

#[test]
fn locally_prepared_compaction_requires_an_explicit_billed_summary_choice() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let temp = tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("local-preview.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("old request")))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("old response".to_owned()),
        Vec::new(),
    )))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("latest request")))?;
    let preview = adaptive_test_compaction_preview(&store)?;
    app.handle_worker_message(WorkerMessage::V2CompactionPreviewed {
        state: V2CompactionPreviewState::Review(Box::new(V2CompactionReview {
            request_id: 45,
            strategy: sigil_kernel::CompactionStrategy::CacheAwareV3,
            preview,
            admission: V2CompactionAdmission::Prepared {
                standalone_tool_output_shrink_available: false,
            },
            tool_output_shrink_candidates: Vec::new(),
            continuity: None,
            native_carrier_requested: false,
        })),
    })?;

    let lines = app.modal_lines().join("\n");
    assert!(lines.contains("local prepare only; no summary/provider request has been sent"));
    assert!(lines.contains("Enter generates one billed semantic summary"));
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        action,
        Some(AppAction::ApplyV2Compaction { request_id: 45 })
    ));
    Ok(())
}

#[test]
fn locally_prepared_compaction_can_choose_standalone_tool_output_cleanup() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let temp = tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("local-shrink-preview.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("old request")))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("old response".to_owned()),
        Vec::new(),
    )))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("latest request")))?;
    let preview = adaptive_test_compaction_preview(&store)?;
    app.handle_worker_message(WorkerMessage::V2CompactionPreviewed {
        state: V2CompactionPreviewState::Review(Box::new(V2CompactionReview {
            request_id: 46,
            strategy: sigil_kernel::CompactionStrategy::CacheAwareV3,
            preview,
            admission: V2CompactionAdmission::Prepared {
                standalone_tool_output_shrink_available: true,
            },
            tool_output_shrink_candidates: Vec::new(),
            continuity: None,
            native_carrier_requested: false,
        })),
    })?;

    assert!(
        app.modal_lines()
            .join("\n")
            .contains("S clean large tool outputs only")
    );
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))?;
    assert!(matches!(
        action,
        Some(AppAction::ApplyStandaloneToolOutputShrink { request_id: 46 })
    ));
    Ok(())
}

#[test]
fn adaptive_compaction_review_shows_protected_tail_and_recoverable_artifact_refs() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let temp = tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("adaptive-preview.jsonl"))?;
    for index in 0..5 {
        store.append(&SessionLogEntry::User(ModelMessage::user(format!(
            "request-{index}"
        ))))?;
        store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
            Some(format!("response-{index}")),
            Vec::new(),
        )))?;
    }
    let preview = store
        .adaptive_compaction_preview(
            sigil_kernel::AdaptiveTailPolicyV3 {
                tail_min_complete_turns: 2,
                tail_target_min_tokens: 64,
                tail_target_max_tokens: 128,
                tail_recent_turn_p95_multiplier_ppm: 2_000_000,
                tail_max_usable_context_ratio_ppm: 250_000,
                recent_turn_sample_limit: 20,
            },
            8 * 1024,
            None,
        )?
        .expect("adaptive fixture should have foldable history");

    app.handle_worker_message(WorkerMessage::V2CompactionPreviewed {
        state: V2CompactionPreviewState::Review(Box::new(V2CompactionReview {
            request_id: 44,
            strategy: sigil_kernel::CompactionStrategy::CacheAwareV3,
            preview,
            admission: V2CompactionAdmission::Unavailable {
                reason: "preview-only fixture".to_owned(),
            },
            tool_output_shrink_candidates: vec![ToolOutputShrinkPreview {
                tool_name: "shell".to_owned(),
                tool_call_id: "call-large".to_owned(),
                status: "ok".to_owned(),
                original_content_bytes: 120_000,
                original_content_token_upper_bound: 120_000,
                head_excerpt: "head".to_owned(),
                tail_excerpt: "tail".to_owned(),
                content_sha256: format!("sha256:{}", "a".repeat(64)),
                artifact_ref: "durable transcript event event-large".to_owned(),
                reason: "large completed historical result".to_owned(),
                recovery_instruction:
                    "Use read_tool_artifact with the opaque artifact ref when omitted details are required."
                        .to_owned(),
            }],
            continuity: Some(V2ContinuityPreview {
                root_objective: "preserve the current implementation objective".to_owned(),
                active_constraints: Vec::new(),
                active_constraint_count: 2,
                authorization_boundary_count: 1,
                recoverable_attachment_count: 1,
                pending_work_count: 3,
                unresolved_question_count: 1,
                source_ref_count: 7,
            }),
            native_carrier_requested: true,
        })),
    })?;

    let lines = app.modal_lines().join("\n");
    assert!(lines.contains("adaptive tail:"));
    assert!(lines.contains("protected tail:"));
    assert!(lines.contains("next-epoch tool artifacts: 1 recoverable candidate"));
    assert!(lines.contains("durable transcript event event-large"));
    assert!(lines.contains("continuity root: preserve the current implementation objective"));
    assert!(lines.contains("7 source ref(s)"));
    assert!(lines.contains("continuity work: 3 pending · 1 unresolved"));
    assert!(lines.contains("native carrier: explicitly requested"));
    Ok(())
}

#[test]
fn dismissed_v2_compaction_review_clears_the_worker_pending_state() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let temp = tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("dismissed-preview.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("old request")))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("old response".to_owned()),
        Vec::new(),
    )))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("latest request")))?;
    let preview = adaptive_test_compaction_preview(&store)?;

    app.handle_worker_message(WorkerMessage::V2CompactionPreviewed {
        state: V2CompactionPreviewState::Review(Box::new(V2CompactionReview {
            request_id: 43,
            strategy: sigil_kernel::CompactionStrategy::CacheAwareV3,
            preview,
            admission: V2CompactionAdmission::Ready {
                before_input_tokens: 200,
                input_tokens: 120,
                context_window_tokens: 1_000_000,
                output_tokens: 32_768,
                safety_buffer_tokens: 8_192,
                savings_tokens: 80,
                savings_ratio_ppm: 400_000,
                minimum_savings_tokens: 64,
                minimum_savings_ratio_ppm: 50_000,
                summary_usage_observed: false,
                deterministic_emergency_fallback: true,
                summary_cache_read_tokens: 0,
                summary_uncached_input_tokens: 0,
                summary_output_tokens: 0,
                summary_cost_nano_usd: None,
                economics_v2: None,
            },
            tool_output_shrink_candidates: Vec::new(),
            continuity: None,
            native_carrier_requested: false,
        })),
    })?;

    let lines = app.modal_lines().join("\n");
    assert!(lines.contains("provider usage unavailable"));
    assert!(lines.contains("audited deterministic emergency floor"));
    assert!(!lines.contains("summary call: cache-read 0"));
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(matches!(
        action,
        Some(AppAction::CancelV2CompactionReview { request_id: 43 })
    ));
    assert!(!app.has_modal());
    assert_eq!(app.last_notice(), Some("closed V2 compaction preview"));
    Ok(())
}

#[test]
fn v2_compaction_apply_renders_one_lifecycle_notice_without_an_assistant_reply() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let assistant_count = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Assistant)
        .count();
    let timeline_count = app.timeline.len();

    app.handle_worker_message(WorkerMessage::V2CompactionApplied {
        request_id: 42,
        source: crate::runner::V2CompactionApplySource::DirectCommand,
        compaction_id: "portable-test-activation".to_owned(),
        folded_event_count: 3,
        entries: vec![SessionLogEntry::User(ModelMessage::user(
            "retained request",
        ))],
    })?;

    assert_eq!(app.timeline.len(), timeline_count + 1);
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        assistant_count
    );
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice
            && entry
                .text
                .contains("Context compacted: 3 message(s) folded")
    }));
    Ok(())
}

#[test]
fn v2_compaction_failure_keeps_the_actionable_reason_in_the_timeline() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle_worker_message(WorkerMessage::V2CompactionApplyFailed {
        request_id: 42,
        error: "insufficient verified savings".to_owned(),
    })?;

    assert_eq!(
        app.last_notice(),
        Some("V2 compaction was not applied: insufficient verified savings")
    );
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice
            && entry.text == "V2 compaction was not applied: insufficient verified savings"
    }));
    Ok(())
}

#[test]
fn idle_auto_compaction_renders_an_automatic_notice_without_an_assistant_reply() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let assistant_count = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Assistant)
        .count();

    app.handle_worker_message(WorkerMessage::V2CompactionApplied {
        request_id: 43,
        source: crate::runner::V2CompactionApplySource::IdleAutomatic,
        compaction_id: "portable-idle-auto-activation".to_owned(),
        folded_event_count: 2,
        entries: vec![SessionLogEntry::User(ModelMessage::user(
            "retained request",
        ))],
    })?;

    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        assistant_count
    );
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice
            && entry
                .text
                .contains("Context compacted automatically: 2 message(s) folded")
    }));
    Ok(())
}

#[test]
fn idle_auto_compaction_rebuilds_the_visible_task_list_from_reloaded_controls() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let task_id = sigil_kernel::TaskId::new("task_compact_survival")?;
    let inspect_step_id = sigil_kernel::TaskStepId::new("inspect")?;
    let implement_step_id = sigil_kernel::TaskStepId::new("implement")?;
    let entries = vec![
        SessionLogEntry::User(ModelMessage::user("continue after automatic compaction")),
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "Preserve the visible task list".to_owned(),
            title: None,
            status: sigil_kernel::TaskRunStatus::Started,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "Preserve the visible task list".to_owned(),
            title: None,
            status: sigil_kernel::TaskRunStatus::Paused,
            reason: Some("waiting for continue".to_owned()),
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 3,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![
                sigil_kernel::TaskStepSpec {
                    step_id: inspect_step_id.clone(),
                    title: "Inspect compacted state".to_owned(),
                    display_name: Some("explorer".to_owned()),
                    detail: None,
                    role: sigil_kernel::AgentRole::SubagentRead,
                    depends_on: Vec::new(),
                    intent_refs: Vec::new(),
                    mode: Some(sigil_kernel::TaskStepMode::Read),
                    isolation: Some(sigil_kernel::TaskIsolationMode::SharedReadOnly),
                },
                sigil_kernel::TaskStepSpec {
                    step_id: implement_step_id.clone(),
                    title: "Resume implementation".to_owned(),
                    display_name: Some("implementer".to_owned()),
                    detail: None,
                    role: sigil_kernel::AgentRole::SubagentWrite,
                    depends_on: vec![inspect_step_id.clone()],
                    intent_refs: Vec::new(),
                    mode: Some(sigil_kernel::TaskStepMode::Write),
                    isolation: Some(sigil_kernel::TaskIsolationMode::Worktree),
                },
            ],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(sigil_kernel::TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 3,
            step_id: inspect_step_id,
            role: sigil_kernel::AgentRole::SubagentRead,
            status: sigil_kernel::TaskStepStatus::Completed,
            title: Some("Inspect compacted state".to_owned()),
            summary: Some("controls retained".to_owned()),
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(sigil_kernel::TaskStepEntry {
            task_id,
            plan_version: 3,
            step_id: implement_step_id,
            role: sigil_kernel::AgentRole::SubagentWrite,
            status: sigil_kernel::TaskStepStatus::Pending,
            title: Some("Resume implementation".to_owned()),
            summary: None,
            reason: None,
        })),
    ];

    app.handle_worker_message(WorkerMessage::V2CompactionApplied {
        request_id: 0,
        source: crate::runner::V2CompactionApplySource::IdleAutomatic,
        compaction_id: "portable-idle-task-survival".to_owned(),
        folded_event_count: 8,
        entries,
    })?;

    let strip = app
        .task_strip_view()
        .expect("automatic compaction should retain the task strip");
    assert_eq!(strip.title, "Preserve the visible task list");
    assert!(strip.detail.contains("paused"));
    assert!(strip.detail.contains("1/2 done"));
    assert!(
        strip
            .rows
            .iter()
            .any(|row| row.label.contains("Inspect compacted state"))
    );
    assert!(
        strip
            .rows
            .iter()
            .any(|row| row.label.contains("Resume implementation"))
    );
    assert!(
        app.task_sidebar_lines()
            .iter()
            .any(|line| line.contains("Resume implementation"))
    );
    Ok(())
}

#[test]
fn pre_turn_compaction_renders_a_lifecycle_notice_before_queue_dispatch() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let assistant_count = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Assistant)
        .count();

    app.handle_worker_message(WorkerMessage::V2CompactionApplied {
        request_id: 0,
        source: crate::runner::V2CompactionApplySource::PreTurnPressure,
        compaction_id: "portable-pre-turn-activation".to_owned(),
        folded_event_count: 2,
        entries: vec![SessionLogEntry::User(ModelMessage::user(
            "queued follow-up",
        ))],
    })?;

    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        assistant_count
    );
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice
            && entry.text.contains(
                "Context compacted before dispatching the queued follow-up: 2 message(s) folded",
            )
    }));
    Ok(())
}

#[test]
fn mcp_activation_status_without_server_name_only_emits_event() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let before = app.runtime.mcp_server_statuses.clone();

    app.handle_worker_message(WorkerMessage::McpActivationStatus {
        server_name: None,
        status: McpActivationStatus::Failed {
            error: "MCP server filesystem tools/list failed: bad response".to_owned(),
        },
    })?;

    assert_eq!(app.runtime.mcp_server_statuses, before);
    assert!(app.mcp_server_runtime_status_label("filesystem").is_none());
    assert!(app.events.iter().any(|event| {
        event.label == "mcp"
            && event.detail.contains("failed")
            && event.detail.contains("bad response")
    }));
    Ok(())
}

#[test]
fn mcp_activate_server_tool_result_marks_lazy_server_ready() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(mcp_server_config! {
        name: "filesystem".to_owned(),
        startup: sigil_kernel::McpServerStartup::Lazy,
        ..sigil_kernel::McpServerConfig::default()
    });
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "activate-filesystem",
        "mcp_activate_server",
        serde_json::json!({
            "server_name": "filesystem",
            "status": "ready",
            "matched_servers": 1,
            "added_tools": 2
        })
        .to_string(),
        Default::default(),
    )))?;

    assert_eq!(
        app.mcp_server_runtime_status_label("filesystem").as_deref(),
        Some("ready 2 tools")
    );
    assert!(app.events.iter().any(|event| {
        event.label == "mcp" && event.detail == "server=filesystem ready tools=2"
    }));
    Ok(())
}

#[test]
fn mcp_runtime_progress_updates_live_activity_without_timeline_notice() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.runtime.is_busy = true;
    app.runtime.run_phase = RunPhase::Tool("mcp__filesystem__scan".to_owned());
    let before_timeline_len = app.timeline.len();

    app.handle_worker_message(WorkerMessage::McpProgress {
        notification: sigil_runtime::McpProgressNotification {
            server_name: "filesystem".to_owned(),
            progress_token: "scan".to_owned(),
            progress: Some(1.0),
            total: Some(4.0),
            message: Some("Scanning".to_owned()),
        },
    })?;

    let summary = app.live_activity_summary().expect("expected mcp progress");
    assert_eq!(summary.label, "mcp");
    assert_eq!(summary.detail, "filesystem: Scanning 25%");
    assert_eq!(app.timeline.len(), before_timeline_len);

    app.handle_worker_message(WorkerMessage::McpProgress {
        notification: sigil_runtime::McpProgressNotification {
            server_name: "filesystem".to_owned(),
            progress_token: "scan".to_owned(),
            progress: Some(7.0),
            total: None,
            message: Some(" ".to_owned()),
        },
    })?;
    let summary = app
        .live_activity_summary()
        .expect("expected progress-only mcp summary");
    assert_eq!(summary.detail, "filesystem: working 7");

    app.handle_worker_message(WorkerMessage::McpProgress {
        notification: sigil_runtime::McpProgressNotification {
            server_name: "filesystem".to_owned(),
            progress_token: "scan".to_owned(),
            progress: None,
            total: None,
            message: None,
        },
    })?;
    let summary = app
        .live_activity_summary()
        .expect("expected message-only mcp summary");
    assert_eq!(summary.detail, "filesystem: working");
    Ok(())
}

#[test]
fn mcp_list_changed_marks_server_stale_until_refresh_status_arrives() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(mcp_server_config! {
        name: "filesystem".to_owned(),
        startup: sigil_kernel::McpServerStartup::Eager,
        ..sigil_kernel::McpServerConfig::default()
    });
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    app.handle_worker_message(WorkerMessage::McpListChanged {
        notification: sigil_runtime::McpListChangedNotification {
            server_name: "filesystem".to_owned(),
            kind: sigil_runtime::McpListChangedKind::Prompts,
        },
    })?;

    assert_eq!(
        app.mcp_server_runtime_status_label("filesystem").as_deref(),
        Some("stale prompts")
    );
    assert_eq!(
        app.last_notice(),
        Some("MCP filesystem prompts changed; refresh queued")
    );
    app.handle_worker_message(WorkerMessage::McpActivationStatus {
        server_name: Some("filesystem".to_owned()),
        status: McpActivationStatus::Refreshing,
    })?;
    assert_eq!(
        app.mcp_server_runtime_status_label("filesystem").as_deref(),
        Some("refreshing")
    );
    Ok(())
}

#[test]
fn run_finished_clears_modal_pending_approval_and_busy_state() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "work".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "work"
    ));
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))?;
    assert!(app.has_modal());
    assert!(app.approval.pending.is_some());
    app.set_pending_user_input(pending_text_user_input_request()?);
    assert!(app.pending_user_input().is_some());

    app.handle_worker_message(WorkerMessage::RunFinished {
        result: sigil_kernel::AgentRunResult {
            final_text: "done".to_owned(),
            tool_calls: 1,
            final_message_id: None,
        },
        entries: restored_entries("deepseek", "deepseek-v4-flash"),
    })?;

    assert!(!app.runtime.is_busy);
    assert_eq!(app.run_phase(), RunPhase::Idle);
    assert!(!app.has_modal());
    assert!(app.approval.pending.is_none());
    assert!(
        app.pending_user_input().is_none(),
        "the returned durable session is authoritative after a continuation settles"
    );
    assert_eq!(app.last_notice(), Some("agent idle"));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "run:finish" && event.detail.contains("tool_calls=1"))
    );
    Ok(())
}

fn worker_terminal_entry(
    task_id: &str,
    status: sigil_kernel::TerminalTaskStatus,
) -> Result<sigil_kernel::TerminalTaskEntry> {
    Ok(sigil_kernel::TerminalTaskEntry {
        schema_version: sigil_kernel::terminal_task::TERMINAL_TASK_SCHEMA_VERSION,
        handle: sigil_kernel::TerminalTaskHandle {
            task_id: sigil_kernel::TerminalTaskId::new(task_id)?,
            command_sha256: "0".repeat(64),
            cwd_label: ".".to_owned(),
            shell_label: "sh".to_owned(),
            shell_sha256: "1".repeat(64),
            log_ref: format!("terminal-log:{task_id}"),
            created_at_ms: 10,
            execution_backend: None,
            execution_backend_capabilities: None,
            enforcement_backend: None,
            enforcement_backend_capabilities: None,
            sandbox_profile: None,
        },
        generation: 1,
        status,
        readiness: sigil_kernel::TerminalReadinessStatus::None,
        output_preview: Some("cancelled output".to_owned()),
        output_hash: Some("hash".to_owned()),
        output_truncated: false,
        output_total_bytes: 0,
        output_limit_bytes: None,
        output_termination_reason: None,
        cleanup: None,
        updated_at_ms: 20,
    })
}
#[test]
fn shell_tool_result_refreshes_visible_workspace_git_status() -> Result<()> {
    let temp = tempdir()?;
    let init = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(temp.path())
        .status()?;
    assert!(init.success());

    let mut config = test_config();
    config.workspace.root = temp.path().display().to_string();
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    assert_eq!(
        app.workspace_git_status()
            .expect("git status")
            .compact_label(),
        "main · clean"
    );

    std::fs::write(temp.path().join("note.txt"), "changed\n")?;
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-git-refresh",
        "bash",
        String::new(),
        ToolResultMeta::default(),
    )))?;

    let status = app.workspace_git_status().expect("refreshed git status");
    assert_eq!(status.branch, "main");
    assert_eq!(status.changed_entries, 1);
    assert_eq!(status.untracked_entries, 1);
    Ok(())
}

#[test]
fn session_restore_rehydrates_the_complete_plan_workbench() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let draft = sigil_kernel::plan_draft_created_entry(
        &structured_plan_text(
            "Restore the durable plan",
            "Inspect the recovery path",
            "crates/sigil-tui/src/app/session_flow.rs",
        ),
        sigil_kernel::PlanSourceRef::default(),
        1,
        None,
    )?
    .expect("plan draft");
    app.restore_session_view(
        Path::new("restored-plan.jsonl").to_path_buf(),
        "mock".to_owned(),
        "mock".to_owned(),
        vec![sigil_kernel::SessionLogEntry::Control(
            sigil_kernel::ControlEntry::PlanDraftCreated(draft.clone()),
        )],
        "restored",
    );

    let pending = app
        .pending_plan_approval()
        .expect("durable pending plan must survive TUI restart");
    assert_eq!(pending.plan_id.as_deref(), Some(draft.plan_id.as_str()));
    assert_eq!(pending.detail.summary, "Restore the durable plan");
    assert_eq!(pending.detail.steps.len(), 1);
    assert!(!pending.workbench_open);
    Ok(())
}

#[test]
fn pending_plan_printable_keys_edit_composer_and_workbench_confirms_save() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let draft = sigil_kernel::plan_draft_created_entry(
        &structured_plan_text("Update README", "Update README.md", "README.md"),
        sigil_kernel::PlanSourceRef::default(),
        1,
        Some("snapshot-1".to_owned()),
    )?
    .expect("non-empty plan should create draft");
    app.set_pending_plan_approval_from_draft(&draft, Some("snapshot-1"));

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.composer.input, "s");
    app.composer.input.clear();
    app.composer.input_cursor = 0;
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
            .is_none()
    );
    assert!(
        app.pending_plan_approval()
            .is_some_and(|plan| plan.workbench_open)
    );
    app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(action, Some(AppAction::SavePlan { .. })));
    Ok(())
}

#[test]
fn plan_workbench_esc_only_closes_and_explicit_revise_emits_action() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let draft = sigil_kernel::plan_draft_created_entry(
        &structured_plan_text("Update README", "Update README.md", "README.md"),
        sigil_kernel::PlanSourceRef::default(),
        1,
        Some("snapshot-1".to_owned()),
    )?
    .expect("non-empty plan should create draft");
    app.set_pending_plan_approval_from_draft(&draft, None);

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
            .is_none()
    );
    let close = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(close.is_none(), "Esc must not reject a plan");
    assert!(
        app.pending_plan_approval()
            .is_some_and(|plan| !plan.workbench_open)
    );
    app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(action, Some(AppAction::RevisePlan { .. })));
    Ok(())
}

#[test]
fn plan_workbench_end_and_up_use_the_rendered_scroll_extent() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let draft = sigil_kernel::plan_draft_created_entry(
        &structured_plan_text("Review a long plan", "Inspect all details", "README.md"),
        sigil_kernel::PlanSourceRef::default(),
        1,
        None,
    )?
    .expect("plan draft");
    app.set_pending_plan_approval_from_draft(&draft, None);
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    app.pending_plan_approval()
        .expect("pending plan")
        .workbench_scroll_extent
        .set(24);

    app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))?;
    assert_eq!(
        app.pending_plan_approval()
            .expect("pending plan")
            .workbench_scroll,
        24
    );
    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(
        app.pending_plan_approval()
            .expect("pending plan")
            .workbench_scroll,
        23,
        "Up must remain usable after End"
    );
    Ok(())
}

#[test]
fn stale_pending_plan_blocks_run_and_save_but_keeps_revise_and_reject() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let draft = sigil_kernel::plan_draft_created_entry(
        &structured_plan_text("Update README", "Update README.md", "README.md"),
        sigil_kernel::PlanSourceRef::default(),
        1,
        Some("base-snapshot".to_owned()),
    )?
    .expect("non-empty plan should create draft");
    app.set_pending_plan_approval_from_draft(&draft, Some("current-snapshot"));

    let pending = app.pending_plan_approval().expect("pending plan");
    assert!(pending.stale);
    assert_eq!(
        pending.workspace_snapshot_id.as_deref(),
        Some("base-snapshot")
    );
    let reason = pending.stale_reason.clone().expect("stale reason");
    assert!(
        reason.contains("stale"),
        "reason must mention staleness: {reason}"
    );

    let open = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(open.is_none(), "Enter opens review before any decision");
    let run = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(run.is_none(), "stale plan must not run");
    assert!(
        app.pending_plan_approval().is_some(),
        "stale plan stays pending"
    );
    assert_eq!(app.last_notice(), Some(reason.as_str()));

    app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    let save = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(save.is_none(), "stale plan must not save");

    app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    let revise = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(revise, Some(AppAction::RevisePlan { .. })));

    app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    let reject = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(reject, Some(AppAction::RejectPlan { .. })));
    Ok(())
}

fn pending_text_user_input_request() -> Result<sigil_kernel::PublicUserInputRequestV1> {
    Ok(sigil_kernel::PublicUserInputRequestV1 {
        identity: sigil_kernel::UserInputIdentityV1 {
            session_scope_id: sigil_kernel::SessionScopeId::new("tui-input-session")?,
            root_logical_run_id: sigil_kernel::LogicalRunId::new("tui-input-root")?,
            source_thread_id: sigil_kernel::AgentThreadId::new("main")?,
            request_id: sigil_kernel::UserInputRequestId::new("tui-input-request")?,
            generation: 1,
            source_binding_hash: format!("sha256:{}", "a".repeat(64)),
        },
        request_hash: format!("sha256:{}", "b".repeat(64)),
        source: sigil_kernel::UserInputSourceV1::Agent,
        purpose: sigil_kernel::UserInputPurposeV1::Clarification,
        prompt: "One constraint is required before work can continue.".to_owned(),
        questions: vec![sigil_kernel::UserInputQuestionV1 {
            id: "scope".to_owned(),
            header: "Scope".to_owned(),
            question: "Which scope should be used?".to_owned(),
            description: None,
            required: true,
            field: sigil_kernel::UserInputFieldKindV1::Text {
                multiline: false,
                max_chars: 64,
            },
        }],
        allowed_actions: vec![
            sigil_kernel::UserInputActionV1::Submit,
            sigil_kernel::UserInputActionV1::Decline,
            sigil_kernel::UserInputActionV1::CancelRun,
        ],
        requested_at_unix_ms: 10,
        status: sigil_kernel::UserInputStatusV1::Requested,
        answer_receipt: None,
        resolution: None,
    })
}

#[test]
fn user_input_form_validates_required_text_preserves_spaces_and_submits_exact_identity()
-> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let request = pending_text_user_input_request()?;
    app.handle_worker_message(WorkerMessage::UserInputRequested {
        request: request.clone(),
        entries: Vec::new(),
    })?;

    assert!(app.pending_user_input().is_some_and(|form| form.open));
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
            .is_none()
    );
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
            .is_none(),
        "an empty required answer must stay in the form"
    );
    assert_eq!(app.last_notice(), Some("Scope requires an answer"));

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    for character in "repo scope".chars() {
        app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        action,
        Some(AppAction::SubmitUserInputDecision {
            command_id: None,
            request_id,
            generation: 1,
            expected_request_hash,
            decision: sigil_kernel::UserInputDecisionV1::Submitted { answers },
        }) if request_id == request.identity.request_id.as_str()
            && expected_request_hash == request.request_hash
            && answers == vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "repo scope".to_owned(),
                },
            }]
    ));
    Ok(())
}

#[test]
fn accepted_user_input_restores_an_exact_resume_action_without_echoing_answers() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let mut request = pending_text_user_input_request()?;
    request.status = sigil_kernel::UserInputStatusV1::DecisionAccepted;
    request.answer_receipt = Some(sigil_kernel::PublicUserInputAnswerReceiptV1 {
        command_id: sigil_kernel::UserInputCommandId::new("desktop-answer-command")?,
        decision: sigil_kernel::PublicUserInputDecisionKindV1::Submitted,
        answer_hash: Some(format!("sha256:{}", "c".repeat(64))),
        answered_question_ids: vec!["scope".to_owned()],
    });
    let command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: request.identity.clone(),
        request_hash: request.request_hash.clone(),
        command_id: sigil_kernel::UserInputCommandId::new("desktop-answer-command")?,
        decision: sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "private accepted value".to_owned(),
                },
            }],
        },
    };
    app.set_pending_user_input_recovery(request.clone(), command.clone());

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        action,
        Some(AppAction::SubmitUserInputDecision {
            command_id: Some(command_id),
            request_id,
            generation: 1,
            expected_request_hash,
            decision,
        }) if command_id == command.command_id.as_str()
            && request_id == request.identity.request_id.as_str()
            && expected_request_hash == request.request_hash
            && decision == command.decision
    ));
    Ok(())
}

#[test]
fn multiline_user_input_uses_enter_for_newline_and_control_enter_for_actions() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let mut request = pending_text_user_input_request()?;
    request.questions[0].field = sigil_kernel::UserInputFieldKindV1::Text {
        multiline: true,
        max_chars: 64,
    };
    app.set_pending_user_input(request);

    for character in "first".chars() {
        app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    for character in "second".chars() {
        app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL))?;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        action,
        Some(AppAction::SubmitUserInputDecision {
            decision: sigil_kernel::UserInputDecisionV1::Submitted { answers },
            ..
        }) if matches!(
            answers.as_slice(),
            [sigil_kernel::UserInputAnswerV1 {
                value: sigil_kernel::UserInputAnswerValueV1::Text { value },
                ..
            }] if value == "first\nsecond"
        )
    ));
    Ok(())
}

#[test]
fn user_input_form_escape_is_repairable_and_never_cancels_the_run() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_pending_user_input(pending_text_user_input_request()?);

    let close = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(close.is_none());
    assert!(app.pending_user_input().is_some_and(|form| !form.open));
    app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))?;
    assert!(app.pending_user_input().is_some_and(|form| form.open));
    Ok(())
}

#[test]
fn user_input_form_page_keys_use_the_rendered_scroll_extent() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_pending_user_input(pending_text_user_input_request()?);
    app.pending_user_input()
        .expect("pending input")
        .scroll_extent
        .set(20);

    app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert_eq!(app.pending_user_input().expect("pending input").scroll, 8);
    app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))?;
    assert_eq!(app.pending_user_input().expect("pending input").scroll, 20);
    app.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    assert_eq!(app.pending_user_input().expect("pending input").scroll, 12);
    app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))?;
    assert_eq!(app.pending_user_input().expect("pending input").scroll, 0);
    Ok(())
}

#[test]
fn user_input_attention_queue_switches_by_exact_identity_and_preserves_drafts() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let first = pending_text_user_input_request()?;
    let mut second = pending_text_user_input_request()?;
    second.identity.request_id = sigil_kernel::UserInputRequestId::new("second-request")?;
    second.identity.source_thread_id = sigil_kernel::AgentThreadId::new("background-child")?;
    second.identity.source_binding_hash = format!("sha256:{}", "d".repeat(64));
    second.request_hash = format!("sha256:{}", "e".repeat(64));
    second.requested_at_unix_ms = first.requested_at_unix_ms + 1;

    app.set_pending_user_input(first.clone());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE))?;
    app.set_pending_user_input(second.clone());
    assert_eq!(
        app.pending_user_input()
            .expect("first request")
            .queue_length,
        2
    );
    assert_eq!(
        app.pending_user_input()
            .and_then(|form| form.request.as_ref())
            .map(|request| &request.identity),
        Some(&first.identity)
    );

    app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?;
    assert_eq!(
        app.pending_user_input()
            .and_then(|form| form.request.as_ref())
            .map(|request| &request.identity),
        Some(&second.identity)
    );
    app.handle_key_event(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;
    assert!(matches!(
        app.pending_user_input().expect("first request").drafts.as_slice(),
        [crate::app::UserInputDraftValue::Text(value)] if value == "a"
    ));
    app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?;
    assert!(matches!(
        app.pending_user_input().expect("second request").drafts.as_slice(),
        [crate::app::UserInputDraftValue::Text(value)] if value == "b"
    ));
    Ok(())
}
