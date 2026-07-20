use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use sigil_kernel::{
    AgentRole, AssistantMessageKind, CandidateCheck, CheckCommand, CheckDiscoverySource,
    CheckPromotion, CheckSpecRecordedEntry, CompletionCriteria, ControlEntry, EvidenceScope,
    NetworkEffect, ReadinessEvaluatedEntry, ReadinessEvaluation, RequiredAction, RunStatus,
    SessionRef, TaskId, TaskPlanEntry, TaskPlanStatus, TaskRunEntry, TaskRunStatus, TaskStepEntry,
    TaskStepId, TaskStepMode, TaskStepSpec, TaskStepStatus, ToolAccess, ToolApproval, ToolCall,
    ToolCategory, ToolEffect, ToolPreviewCapability, ToolSpec, VerificationPolicy,
    VerificationPolicyChangedEntry, VerificationProductAction, VerificationVerdict,
    VisibleCompletionState, build_workspace_snapshot, stable_workspace_id,
};

use super::*;
use crate::{
    HttpDurableEgressDisclosureJournal, HttpDurableProtocolJournal, HttpPermissionMode,
    HttpRunStartRequest, HttpRunStatus, HttpSessionCreateRequest, HttpSessionOpenRequest,
};

fn call() -> ToolCall {
    ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: r#"{"path":"README.md"}"#.to_owned(),
    }
}

fn spec(access: ToolAccess, network_effect: Option<NetworkEffect>) -> ToolSpec {
    ToolSpec {
        name: "read_file".to_owned(),
        description: "read a file".to_owned(),
        input_schema: serde_json::json!({"type":"object"}),
        category: ToolCategory::File,
        access,
        network_effect,
        preview: ToolPreviewCapability::None,
    }
}

struct ControlledPreparation {
    started: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
}

#[async_trait]
impl HttpApplicationRunPreparer for ControlledPreparation {
    async fn prepare(
        &self,
        _request: ApplicationRunRequest,
        _services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        self.started.add_permits(1);
        self.release
            .acquire()
            .await
            .map_err(|_| anyhow!("controlled preparation release closed"))?
            .forget();
        Err(anyhow!(
            "controlled preparation released after cancellation"
        ))
    }
}

#[test]
fn approval_broker_routes_one_explicit_decision_with_stable_guards() {
    let broker = Arc::new(HttpApprovalBroker::default());
    let pending = broker
        .register(
            "run-1",
            &call(),
            &spec(ToolAccess::Read, None),
            Duration::from_secs(1),
        )
        .expect("approval should register");
    assert_eq!(pending.policy_version, HTTP_APPROVAL_POLICY_VERSION);
    assert!(pending.approval_request_id.starts_with("http-approval-v1:"));
    assert_eq!(pending.tool_call_hash.len(), 64);

    broker
        .resolve(
            "call-1",
            HttpApprovalDecisionRecord {
                run_id: "run-1".to_owned(),
                call_id: "call-1".to_owned(),
                decision: ToolApprovalUserDecision::Approved,
                reason: None,
            },
        )
        .expect("decision should resolve");
    let outcome = broker
        .wait_for_decision("call-1")
        .expect("resolved wait should finish");

    assert!(!outcome.expired);
    assert!(matches!(
        outcome.decision,
        Some(HttpApprovalDecisionRecord {
            decision: ToolApprovalUserDecision::Approved,
            ..
        })
    ));
}

#[test]
fn approval_broker_expires_and_cleans_up_without_fabricating_a_decision() {
    let broker = HttpApprovalBroker::default();
    broker
        .register(
            "run-1",
            &call(),
            &spec(ToolAccess::Read, None),
            Duration::ZERO,
        )
        .expect("approval should register");

    let outcome = broker
        .wait_for_decision("call-1")
        .expect("expiry should be a typed denial path");

    assert!(outcome.expired);
    assert!(outcome.decision.is_none());
    assert!(
        broker
            .pending
            .lock()
            .expect("broker should lock")
            .is_empty()
    );
}

#[test]
fn approval_handler_only_resolves_explicit_broker_decisions() {
    let broker = Arc::new(HttpApprovalBroker::default());
    broker
        .register(
            "run-1",
            &call(),
            &spec(ToolAccess::Write, None),
            Duration::from_secs(1),
        )
        .expect("approval should register");
    broker
        .resolve(
            "call-1",
            HttpApprovalDecisionRecord {
                run_id: "run-1".to_owned(),
                call_id: "call-1".to_owned(),
                decision: ToolApprovalUserDecision::Approved,
                reason: None,
            },
        )
        .expect("decision should resolve");
    let mut handler = HttpProductionApprovalHandler {
        run_id: "run-1".to_owned(),
        registry: Weak::new(),
        broker,
    };

    assert!(matches!(
        handler
            .approve_tool_call(&call(), &spec(ToolAccess::Write, None))
            .expect("explicit decision should resolve"),
        ToolApproval::Approve
    ));
    assert!(handler.approval_is_explicit_user_action());
}

#[tokio::test]
async fn production_driver_rejects_an_in_memory_only_event_bus() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
            .expect("disclosure journal should initialize"),
    );

    assert!(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new("sigil.toml", "."),
            disclosure_journal,
            Arc::new(HttpLiveEventBus::new(8)),
            tokio::runtime::Handle::current(),
        )
        .is_err()
    );
}

#[tokio::test]
async fn production_driver_session_reopen_revalidates_lifecycle_and_durable_truth() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )
    .expect("test config should write");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir(&sessions).expect("session directory should create");
    let session_path = sessions.join("session-history.jsonl");
    let store = sigil_kernel::JsonlSessionStore::new(&session_path)
        .expect("durable session store should open");
    let mut session = sigil_kernel::Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session
        .append_user_message(sigil_kernel::ModelMessage::user("history"))
        .expect("durable message should append");
    session
        .append_assistant_message(sigil_kernel::ModelMessage::assistant_with_kind(
            Some("durable answer".to_owned()),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        ))
        .expect("durable assistant should append");
    let durable_session_id = session.session_scope_id().to_owned();
    drop(session);

    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 8)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(8, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
            .expect("disclosure journal should initialize"),
    );
    let lifecycle = sigil_runtime::LocalSessionLifecycleService::new(
        "workspace-1",
        &sessions,
        temp.path().join("exports"),
    );
    let options = HttpProductionRunDriverOptions::new(&config_path, temp.path())
        .with_session_lifecycle(lifecycle);
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            options,
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should initialize"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands.json"), 8)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let request = HttpSessionOpenRequest {
        session_ref: "session-history.jsonl".to_owned(),
        session_id: durable_session_id.clone(),
        label: Some("History".to_owned()),
    };

    let opened = registry
        .open_session(request.clone())
        .expect("current durable source should reopen");

    assert_eq!(opened.durable_session_scope_id, durable_session_id);
    let transcript = registry
        .transcript_page(&opened.id, None, 50)
        .expect("production transcript should project");
    assert_eq!(transcript.session_scope_id, durable_session_id);
    assert_eq!(transcript.total_messages, 2);
    assert_eq!(
        transcript.messages[1].content.as_deref(),
        Some("durable answer")
    );
    assert_eq!(
        transcript.messages[1].assistant_kind,
        Some(crate::HttpTranscriptAssistantKind::FinalAnswer)
    );
    assert_eq!(
        std::path::Path::new(&opened.session_log_path),
        session_path
            .canonicalize()
            .expect("session path should resolve")
    );
    assert_eq!(
        registry
            .open_session(request)
            .expect("duplicate reopen should be idempotent")
            .id,
        opened.id
    );
    assert_eq!(
        registry.open_session(HttpSessionOpenRequest {
            session_ref: "session-history.jsonl".to_owned(),
            session_id: "stale-id".to_owned(),
            label: None,
        }),
        Err(crate::HttpRegistryError::DurableSessionIdentityChanged)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_driver_projects_and_executes_real_verification_rerun() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace should create");
    std::fs::write(workspace.join("note.txt"), "current\n").expect("fixture should write");
    let workspace = workspace.canonicalize().expect("workspace should resolve");
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "workspace"

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )
    .expect("test config should write");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 16)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(8, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
            .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(&config_path, temp.path()),
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should initialize"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands.json"), 8)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let adapter_session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("session should bind");
    let store = sigil_kernel::JsonlSessionStore::new(&adapter_session.session_log_path)
        .expect("session store should open");
    let mut session =
        sigil_kernel::Session::load_from_store("deepseek", "deepseek-v4-flash", store)
            .expect("session should load");
    let task_id = TaskId::new("task_1").expect("task id");
    let step_id = TaskStepId::new("verify_1").expect("step id");
    let scope = EvidenceScope::Step("task_1:verify_1".to_owned());
    session
        .append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl").expect("session ref"),
            objective: "verify workspace".to_owned(),
            status: TaskRunStatus::Paused,
            reason: None,
        }))
        .expect("task run should append");
    session
        .append_control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id.clone(),
                title: "verify".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                mode: Some(TaskStepMode::Verify),
                isolation: None,
            }],
            reason: None,
        }))
        .expect("task plan should append");
    session
        .append_control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: step_id.clone(),
            role: AgentRole::Executor,
            status: TaskStepStatus::Blocked,
            title: Some("verify".to_owned()),
            summary: None,
            reason: None,
        }))
        .expect("task step should append");
    let trusted = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "rustc".to_owned(),
            args: vec!["--version".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "rustc-version",
        "task_step_default",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )
    .expect("configured check should promote");
    let check_spec = trusted.check_spec.clone();
    session
        .append_control(ControlEntry::CheckSpecRecorded(
            CheckSpecRecordedEntry::new(
                EvidenceScope::Task(task_id.as_str().to_owned()),
                trusted,
                "event-config",
            ),
        ))
        .expect("check spec should append");
    let mut policy = VerificationPolicy::no_checks_required("task_step_default");
    policy.required_checks = vec![check_spec.clone()];
    policy.completion_criteria = CompletionCriteria::AllRequiredChecks;
    policy.allow_unverified_completion = false;
    policy.timeout_ms = Some(60_000);
    let policy_entry = VerificationPolicyChangedEntry::new(
        EvidenceScope::Task(task_id.as_str().to_owned()),
        policy.clone(),
        "event-policy",
    )
    .expect("policy should hash");
    let policy_hash = policy_entry.policy_hash.clone();
    session
        .append_control(ControlEntry::VerificationPolicyChanged(policy_entry))
        .expect("policy should append");
    let workspace_id = stable_workspace_id(&workspace).expect("workspace id");
    let snapshot =
        build_workspace_snapshot(&workspace, workspace_id, &policy.verification_scope, 0)
            .expect("workspace should snapshot");
    let snapshot_id = snapshot
        .workspace_snapshot_id
        .expect("snapshot should have identity");
    session
        .append_control(ControlEntry::ReadinessEvaluated(ReadinessEvaluatedEntry {
            scope,
            evaluation: ReadinessEvaluation {
                run_status: RunStatus::Completed,
                verification_verdict: VerificationVerdict::Missing,
                visible_state: VisibleCompletionState::CompletedUnverified,
                reasons: Vec::new(),
                required_actions: vec![RequiredAction::RunCheck {
                    check_spec_id: check_spec.check_spec_id.clone(),
                }],
            },
            policy_hash: Some(policy_hash),
            workspace_snapshot_id: Some(snapshot_id),
        }))
        .expect("readiness should append");
    drop(session);

    let rendered = registry
        .verification_view(&adapter_session.id)
        .expect("verification should project")
        .expect("verification should exist");
    let VerificationProductAction::Rerun(request) = rendered.action.expect("rerun action") else {
        panic!("expected exact rerun action");
    };
    let command = crate::HttpCommandEnvelope::new(
        "verification-real-1",
        "desktop-test",
        &adapter_session.id,
        request,
    );
    let registry_for_rerun = Arc::clone(&registry);
    let session_id = adapter_session.id.clone();
    let receipt = tokio::task::spawn_blocking(move || {
        registry_for_rerun.rerun_verification_command(&session_id, command)
    })
    .await
    .expect("rerun worker should join")
    .expect("real verification should execute");

    assert_eq!(receipt.verification.status, "passed");
    assert!(receipt.verification.action.is_none());
    assert_eq!(
        receipt.verification.evidence.check_status,
        Some(sigil_kernel::VerificationCheckRunStatus::Succeeded)
    );
    assert!(receipt.verification.evidence.receipt_id.is_some());
    assert!(
        receipt
            .verification
            .evidence
            .workspace_snapshot_id
            .is_some()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preparation_deadline_quarantines_before_ack_and_retains_the_owner_for_reaping() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )
    .expect("test config should write");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 16)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(8, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
            .expect("disclosure journal should initialize"),
    );
    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let mut options = HttpProductionRunDriverOptions::new(&config_path, temp.path());
    options.cancellation_timeout = Duration::from_millis(40);
    let driver = Arc::new(
        HttpProductionRunDriver::new_with_preparer(
            options,
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
            Arc::new(ControlledPreparation {
                started: Arc::clone(&started),
                release: Arc::clone(&release),
            }),
        )
        .expect("production driver should initialize"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands.json"), 16)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("session should bind");
    let run = registry
        .start_run(
            &session.id,
            HttpRunStartRequest {
                prompt: "wait in preparation".to_owned(),
                permission_mode: Some(HttpPermissionMode::Manual),
                reasoning_effort: None,
                reasoning_effort_binding: None,
                skill_binding: None,
            },
        )
        .expect("run should start");
    started
        .acquire()
        .await
        .expect("preparation should start")
        .forget();

    let cancel_registry = Arc::clone(&registry);
    let run_id = run.id.clone();
    let cancel = tokio::task::spawn_blocking(move || cancel_registry.cancel_run(&run_id));
    let result = tokio::time::timeout(Duration::from_millis(400), cancel)
        .await
        .expect("cancel caller must return at the configured deadline")
        .expect("cancel worker should join");
    assert!(matches!(
        result,
        Err(crate::HttpRegistryError::DriverRejected {
            operation: "cancel",
            ..
        })
    ));
    assert_eq!(
        registry.get_run(&run.id).expect("run should exist").status,
        HttpRunStatus::ExecutionUncertain
    );
    assert_eq!(
        driver
            .active_run_count()
            .expect("active owners should remain observable"),
        1,
        "the timed-out preparation owner must remain held until it is reaped"
    );

    release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if driver.active_run_count().expect("active runs should read") == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("released preparation should be reaped");
    assert_eq!(
        registry.get_run(&run.id).expect("run should exist").status,
        HttpRunStatus::ExecutionUncertain
    );
}

#[test]
fn approval_protocol_event_exposes_the_exact_guard_required_by_the_endpoint() {
    let bus = HttpLiveEventBus::new(8);
    let call = call();
    let spec = spec(ToolAccess::Write, None);
    let pending = HttpPendingApproval {
        call_id: call.id.clone(),
        tool_name: spec.name.clone(),
        approval_request_id: format!("http-approval-v1:{}", "a".repeat(64)),
        tool_call_hash: "b".repeat(64),
        policy_version: HTTP_APPROVAL_POLICY_VERSION.to_owned(),
        expires_at_ms: 10,
    };
    let event = PublicRunEvent::new(
        "durable-session-1",
        "run-1",
        1,
        PublicRunEventKind::ApprovalRequested {
            call,
            spec,
            subjects: Vec::new(),
            network_effect: None,
            local_policy_decision: None,
            network_policy_decision: None,
            source_policy_decision: None,
            operation: None,
            risk: None,
            subject_zones: Vec::new(),
            confirmation: None,
            snapshot_required: false,
            command_permission_matches: Vec::new(),
            preview: None,
        },
    );

    let published = bus
        .publish_run_event_with_approval(event, Some(pending.clone()))
        .expect("matching HTTP approval guard should publish");

    assert_eq!(published.approval_request, Some(pending));
    assert!(matches!(
        published.view(),
        crate::HttpProtocolEventView::Durable(crate::HttpDurableEventView {
            approval_request: Some(_),
            ..
        })
    ));
}

#[test]
fn approval_protocol_event_rejects_guard_for_another_call() {
    let bus = HttpLiveEventBus::new(8);
    let call = call();
    let spec = spec(ToolAccess::Write, None);
    let event = PublicRunEvent::new(
        "durable-session-1",
        "run-1",
        1,
        PublicRunEventKind::ApprovalRequested {
            call,
            spec,
            subjects: Vec::new(),
            network_effect: None,
            local_policy_decision: None,
            network_policy_decision: None,
            source_policy_decision: None,
            operation: None,
            risk: None,
            subject_zones: Vec::new(),
            confirmation: None,
            snapshot_required: false,
            command_permission_matches: Vec::new(),
            preview: None,
        },
    );
    let wrong = HttpPendingApproval {
        call_id: "call-other".to_owned(),
        tool_name: "read_file".to_owned(),
        approval_request_id: format!("http-approval-v1:{}", "a".repeat(64)),
        tool_call_hash: "b".repeat(64),
        policy_version: HTTP_APPROVAL_POLICY_VERSION.to_owned(),
        expires_at_ms: 10,
    };

    assert!(matches!(
        bus.publish_run_event_with_approval(event, Some(wrong)),
        Err(crate::HttpEventPublishError::ApprovalMetadata)
    ));
}

#[tokio::test]
async fn production_driver_uses_shared_runtime_preparation_and_records_typed_failure() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )
    .expect("test config should write");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 32)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 16)
            .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(&config_path, temp.path()),
            disclosure_journal,
            Arc::clone(&event_bus),
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should accept a durable event bus"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands.json"), 32)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("durable session binding should not require provider assembly");
    let run = registry
        .start_run(
            &session.id,
            HttpRunStartRequest {
                prompt: "hello".to_owned(),
                permission_mode: Some(HttpPermissionMode::Manual),
                reasoning_effort: None,
                reasoning_effort_binding: None,
                skill_binding: None,
            },
        )
        .expect("owned production supervisor should accept the run");

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let status = registry
                .get_run(&run.id)
                .expect("run should remain addressable")
                .status;
            if status.is_terminal() {
                break status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("preparation failure should terminate promptly");

    assert_eq!(
        registry.get_run(&run.id).expect("run should exist").status,
        HttpRunStatus::Failed
    );
    assert!(session.session_log_path.ends_with(".jsonl"));
    let replay = event_bus
        .replay_run_after(&session.durable_session_scope_id, &run.id, None)
        .expect("typed preparation failure should be durable");
    assert!(matches!(
        replay.last().map(|event| &event.run_event.event),
        Some(PublicRunEventKind::RunFailed { .. })
    ));
}

#[tokio::test]
async fn production_cancel_returns_only_after_supervisor_acknowledges_activation() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 8)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(8, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
            .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(temp.path().join("sigil.toml"), temp.path()),
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should accept a durable event bus"),
    );
    let (cancel_sender, mut cancel_receiver) = mpsc::unbounded_channel();
    driver
        .active_runs
        .lock()
        .expect("active runs should lock")
        .insert(
            "run-1".to_owned(),
            Arc::new(HttpProductionActiveRun {
                session_id: "session-1".to_owned(),
                broker: Arc::new(HttpApprovalBroker::default()),
                cancel_sender,
            }),
        );
    let (finished, finished_rx) = std_mpsc::channel();
    let cancel_driver = Arc::clone(&driver);
    let caller = std::thread::spawn(move || {
        let result = cancel_driver.cancel_run(HttpRunDriverCancel {
            session_id: "session-1".to_owned(),
            run_id: "run-1".to_owned(),
            reason: Some("user requested stop".to_owned()),
        });
        finished
            .send(())
            .expect("completion signal should be delivered");
        result
    });
    let command = cancel_receiver
        .recv()
        .await
        .expect("supervisor should receive cancellation");

    assert_eq!(command.reason, "user requested stop");
    assert!(finished_rx.try_recv().is_err());
    command
        .acknowledgement
        .send(Ok(()))
        .expect("durable activation acknowledgement should send");
    finished_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("driver call should finish after acknowledgement");
    caller
        .join()
        .expect("cancel caller should join")
        .expect("acknowledged cancellation should succeed");
}
