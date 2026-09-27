//! Scripted model decisions drive real managed file tools and real verification subprocesses.
use super::*;

#[derive(Clone, Copy)]
enum FeedbackMode {
    Final,
    Repair,
    Blocked,
    Input,
    Repeat,
    Stale,
    WrongOrder,
    Outside,
    Adaptive,
    MultipleChecks,
}

struct FeedbackProvider {
    mode: FeedbackMode,
    calls: Arc<AtomicUsize>,
    requests: Arc<std::sync::Mutex<Vec<CompletionRequest>>>,
    workspace: std::path::PathBuf,
}

struct FeedbackBuilder {
    mode: FeedbackMode,
    calls: Arc<AtomicUsize>,
    requests: Arc<std::sync::Mutex<Vec<CompletionRequest>>>,
    workspace: std::path::PathBuf,
}

#[async_trait]
impl TaskRoleProviderBuilder for FeedbackBuilder {
    async fn build(&self, _config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        assert_eq!(role, AgentRole::Executor);
        Ok(Box::new(FeedbackProvider {
            mode: self.mode,
            calls: self.calls.clone(),
            requests: self.requests.clone(),
            workspace: self.workspace.clone(),
        }))
    }
}

fn call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        args_json: args.to_string(),
    }
}

fn tool_chunks(calls: Vec<ToolCall>) -> Vec<Result<ProviderChunk>> {
    let mut chunks = Vec::new();
    for call in calls {
        chunks.push(Ok(ProviderChunk::ToolCallStart {
            id: call.id.clone(),
            name: call.name.clone(),
        }));
        chunks.push(Ok(ProviderChunk::ToolCallArgsDelta {
            id: call.id.clone(),
            delta: call.args_json.clone(),
        }));
        chunks.push(Ok(ProviderChunk::ToolCallComplete(call)));
    }
    chunks.push(Ok(ProviderChunk::Done));
    chunks
}

#[async_trait]
impl Provider for FeedbackProvider {
    fn name(&self) -> &str {
        "application-task-test"
    }
    fn capabilities(&self) -> ProviderCapabilities {
        application_task_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .expect("requests lock")
            .push(request.clone());
        let repair = || {
            call(
                &format!("repair-{index}"),
                "respond_to_task_verification",
                serde_json::json!({"decision":"repair"}),
            )
        };
        let blocked = || {
            call(
                &format!("blocked-{index}"),
                "respond_to_task_verification",
                serde_json::json!({"decision":"blocked", "reason":"The remaining failure needs a separate decision."}),
            )
        };
        let edit = |path: &str| {
            call(
                &format!("edit-{index}"),
                "write_file",
                serde_json::json!({"path":path,"content":"valid\n"}),
            )
        };
        let calls = match (self.mode, index) {
            (FeedbackMode::Repair | FeedbackMode::Adaptive, 1) => {
                Some(vec![repair(), edit("src/input")])
            }
            (FeedbackMode::Adaptive, index) if index >= 2 => {
                (tokio::fs::read_to_string(self.workspace.join("src/input")).await? != "valid\n")
                    .then(|| vec![edit("src/input")])
            }
            (FeedbackMode::WrongOrder, 1) => Some(vec![edit("src/input"), repair()]),
            (FeedbackMode::Outside, 1) => Some(vec![repair(), edit("../outside")]),
            (FeedbackMode::Stale, 1) => {
                tokio::fs::write(self.workspace.join("src/input"), "external change\n").await?;
                Some(vec![repair(), edit("src/input")])
            }
            (FeedbackMode::Blocked, 1)
            | (FeedbackMode::Stale | FeedbackMode::WrongOrder | FeedbackMode::Outside, 3) => {
                Some(vec![blocked()])
            }
            (FeedbackMode::Input, 1) => Some(vec![call(
                "need-input",
                "request_user_input",
                serde_json::json!({"questions":[{"id":"scope","question":"May this task change the expected behavior?"}]}),
            )]),
            (FeedbackMode::Repeat, index) if index % 2 == 1 => Some(vec![repair()]),
            (FeedbackMode::MultipleChecks, index) if index % 2 == 1 => Some(vec![
                repair(),
                call(
                    &format!("edit-{index}"),
                    "write_file",
                    serde_json::json!({"path":"src/input", "content": format!("{}\n", index.div_ceil(2))}),
                ),
            ]),
            _ => None,
        };
        let chunks = calls.map(tool_chunks).unwrap_or_else(|| {
            scripted_task_completion_chunks(
                &request,
                "Candidate completion; use actual verification evidence.",
                "feedback-final",
            )
        });
        Ok(Box::pin(stream::iter(chunks)))
    }
}

struct FeedbackFixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    path: std::path::PathBuf,
    config_path: std::path::PathBuf,
    identity: String,
    task: TaskId,
    services: ApplicationRunServices,
    calls: Arc<AtomicUsize>,
    requests: Arc<std::sync::Mutex<Vec<CompletionRequest>>>,
}

impl FeedbackFixture {
    async fn new(mode: FeedbackMode, source: &str, command: Option<&str>) -> Result<Self> {
        Self::new_with_live_config(mode, source, command, None).await
    }

    async fn new_with_live_config(
        mode: FeedbackMode,
        source: &str,
        command: Option<&str>,
        live_config: Option<&Path>,
    ) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(root.join("src"))?;
        std::fs::write(root.join("src/input"), source)?;
        std::fs::write(temp.path().join("outside"), "outside sentinel")?;
        let (config_path, services) = services(&root, false)?;
        if matches!(mode, FeedbackMode::Repeat) {
            let mut config = RootConfig::load(&config_path)?;
            config.agent.max_turns = Some(4);
            std::fs::write(&config_path, config.persisted_toml()?)?;
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let builder: Arc<dyn TaskRoleProviderBuilder> = if let Some(source_config) = live_config {
            let mut config = RootConfig::load_persisted(&config_path)?;
            let live = RootConfig::load_persisted(source_config)?;
            // Import only the explicitly selected provider setup. The isolated fixture owns
            // storage, composition and optional module documents, including deferred values.
            config.connections = live.connections;
            config.agent.connection = live.agent.connection;
            config.agent.runtime_provider = live.agent.runtime_provider;
            config.agent.model = live.agent.model;
            config.model_request = live.model_request;
            config.workspace.root = root.display().to_string();
            config.agent.max_turns = Some(12);
            config.model_request.max_output_tokens = Some(4096);
            std::fs::write(&config_path, config.persisted_toml()?)?;
            Arc::new(FirstFinalThenLiveBuilder {
                calls: calls.clone(),
            })
        } else {
            Arc::new(FeedbackBuilder {
                mode,
                calls: calls.clone(),
                requests: requests.clone(),
                workspace: root.clone(),
            })
        };
        let services = services.with_task_role_provider_builder(builder);
        let mut prepared = Box::pin(prepare_application_run(
            ApplicationRunRequest::non_interactive(
                &config_path,
                &root,
                "Repair src/input until its declared check passes",
                "feedback-seed",
            ),
            &services,
        ))
        .await?;
        let path = prepared.session_log_path().to_path_buf();
        let identity = prepared.session_id().to_owned();
        let task = TaskId::new("verification-feedback-task")?;
        let parent = prepared.execution.parent_session_ref.clone();
        let objective = "Repair src/input until its declared check passes";
        let admission =
            crate::direct_plan_fixture::append(&mut prepared.execution.session, &task, objective)?;
        prepared.execution.session.append_controls(vec![
            ControlEntry::TaskRun(TaskRunEntry {
                task_id: task.clone(),
                parent_session_ref: parent,
                objective: objective.to_owned(),
                title: None,
                status: TaskRunStatus::Paused,
                reason: None,
            }),
            ControlEntry::TaskDirectExecutionAdmittedV1(admission),
        ])?;
        if let Some(command) = command {
            check(
                &mut prepared.execution.session,
                &task,
                CheckCommand {
                    command: "python3".to_owned(),
                    args: vec!["-c".to_owned(), command.to_owned()],
                    cwd: None,
                },
                VerificationAutoRunPolicy::TrustedOnly,
            )?;
        }
        drop(prepared);
        Ok(Self {
            _temp: temp,
            root,
            path,
            config_path,
            identity,
            task,
            services,
            calls,
            requests,
        })
    }

    async fn prepare(
        &self,
        run_id: &str,
    ) -> Result<crate::application_run::PreparedApplicationTaskContinuation> {
        Ok(Box::pin(prepare_application_task_continuation(
            ApplicationTaskContinuationRequest {
                config_path: self.config_path.clone(),
                launch_cwd: self.root.clone(),
                session_path: self.path.clone(),
                session_attachment: None,
                expected_session_scope_id: self.identity.clone(),
                run_id: run_id.to_owned(),
                task_id: self.task.clone(),
                guidance: None,
                interaction: ApplicationRunInteraction::NonInteractive,
                permission_mode: None,
            },
            &self.services,
        ))
        .await?)
    }

    fn session(&self) -> Result<Session> {
        Session::load_from_store(
            "application-task-test",
            "gpt-test",
            JsonlSessionStore::new(&self.path)?,
        )
    }
}

const CHECK_SOURCE: &str = "from pathlib import Path; import sys; ok = Path('src/input').read_text().strip() == 'valid'; print('src/input:1: expected valid' if not ok else 'check passed', file=sys.stderr); sys.exit(0 if ok else 1)";

struct CaseResult {
    fixture: FeedbackFixture,
    session: Session,
    status: TaskRunStatus,
    calls: usize,
    requests: Vec<CompletionRequest>,
    source: String,
    outside: String,
}

async fn run_feedback_case(
    mode: FeedbackMode,
    source: &str,
    configured: bool,
) -> Result<CaseResult> {
    let fixture = FeedbackFixture::new(mode, source, configured.then_some(CHECK_SOURCE)).await?;
    let (execution, control) = fixture.prepare("feedback-run").await?.into_parts();
    let output = Box::pin(execution.execute(
        &mut RecordingApplicationRunEvents::default(),
        &mut AutoApproveHandler,
    ))
    .await?;
    drop(control);
    let requests = fixture.requests.lock().expect("requests lock").clone();
    Ok(CaseResult {
        session: fixture.session()?,
        status: output.task_status,
        calls: fixture.calls.load(Ordering::SeqCst),
        requests,
        source: std::fs::read_to_string(fixture.root.join("src/input"))?,
        outside: std::fs::read_to_string(fixture._temp.path().join("outside"))?,
        fixture,
    })
}

fn receipt_statuses(session: &Session) -> Vec<ReceiptStatus> {
    session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::VerificationRecorded(entry)) => {
                Some(entry.receipt.check_status)
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn task_verification_feedback_repairs_and_rechecks_with_no_decision_only_round() -> Result<()>
{
    let _environment_guard = crate::test_env::lock();
    let result = run_feedback_case(FeedbackMode::Repair, "broken\n", true).await?;
    assert_eq!(result.status, TaskRunStatus::Completed);
    assert_eq!(
        result.calls, 3,
        "candidate final, repair+write, verified final"
    );
    assert_eq!(result.source, "valid\n");
    assert_eq!(
        receipt_statuses(&result.session),
        [ReceiptStatus::Failed, ReceiptStatus::Succeeded]
    );
    let feedback = serde_json::to_string(&result.requests[1].messages)?;
    assert!(
        feedback.contains("src/input:1: expected valid"),
        "actual check output must reach the model"
    );
    assert!(feedback.contains("check_spec_hash"));
    assert!(result.session.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::TaskVerificationFeedbackV1(feedback))
            if feedback.status == sigil_kernel::TaskVerificationFeedbackStatusV1::Repair)));
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn task_verification_feedback_success_and_no_checks_need_one_model_call() -> Result<()> {
    let _environment_guard = crate::test_env::lock();
    for configured in [false, true] {
        let result = run_feedback_case(FeedbackMode::Final, "valid\n", configured).await?;
        assert_eq!(result.status, TaskRunStatus::Completed);
        assert_eq!(result.calls, 1);
        assert_eq!(
            receipt_statuses(&result.session),
            if configured {
                vec![ReceiptStatus::Succeeded]
            } else {
                Vec::new()
            }
        );
        assert!(
            !result.requests[0]
                .tools
                .iter()
                .any(|tool| tool.name == "respond_to_task_verification")
        );
    }
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn task_verification_feedback_blocked_and_input_remain_paused() -> Result<()> {
    let _environment_guard = crate::test_env::lock();
    for mode in [FeedbackMode::Blocked, FeedbackMode::Input] {
        let result = run_feedback_case(mode, "broken\n", true).await?;
        assert_eq!(result.status, TaskRunStatus::Paused);
        assert_eq!(result.calls, 2);
        assert_eq!(receipt_statuses(&result.session), [ReceiptStatus::Failed]);
        assert_eq!(result.source, "broken\n");
    }
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn task_verification_feedback_repeated_failures_are_bounded_and_never_completed() -> Result<()>
{
    let _environment_guard = crate::test_env::lock();
    let result = run_feedback_case(FeedbackMode::Repeat, "broken\n", true).await?;
    assert_eq!(result.status, TaskRunStatus::Paused);
    assert_eq!(
        result.calls, 4,
        "the original configured run turn budget bounds unchanged repairs"
    );
    assert_eq!(
        receipt_statuses(&result.session),
        vec![ReceiptStatus::Failed; 2]
    );
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn task_verification_feedback_stale_choice_wrong_order_and_external_write_do_not_gain_authority()
-> Result<()> {
    let _environment_guard = crate::test_env::lock();
    for mode in [
        FeedbackMode::Stale,
        FeedbackMode::WrongOrder,
        FeedbackMode::Outside,
    ] {
        let result = run_feedback_case(mode, "broken\n", true).await?;
        assert_eq!(result.status, TaskRunStatus::Paused);
        assert_ne!(result.source, "valid\n");
        assert_eq!(result.outside, "outside sentinel");
        assert!(
            receipt_statuses(&result.session)
                .iter()
                .all(|status| *status == ReceiptStatus::Failed)
        );
    }
    Ok(())
}

// Keep an exact, genuinely emitted durable prefix: this simulates power loss, not a fabricated
// verification receipt. All live writer owners are dropped before replacing the isolated fixture.
fn keep_feedback_crash_prefix(fixture: &FeedbackFixture, dispatched: bool) -> Result<()> {
    let records = JsonlSessionStore::read_event_records(&fixture.path)?;
    let boundary = records.iter().position(|record| {
        matches!(record.session_log_entry(), Ok(Some(SessionLogEntry::Control(ControlEntry::TaskVerificationFeedbackV1(feedback)))) if feedback.status == sigil_kernel::TaskVerificationFeedbackStatusV1::Pending)
    }).expect("real pending feedback");
    let last = if dispatched {
        records
            .iter()
            .enumerate()
            .skip(boundary + 1)
            .find_map(|(index, record)| {
                (record.stored_event().event_type
                    == sigil_kernel::DurableEventType::ProviderPhysicalAttemptStarted.as_str())
                .then_some(index)
            })
            .expect("real feedback provider dispatch")
    } else {
        boundary
    };
    let original = std::fs::read_to_string(&fixture.path)?;
    assert_eq!(original.lines().count(), records.len());
    let prefix = original
        .lines()
        .take(last + 1)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&fixture.path, prefix)?;
    std::fs::write(fixture.root.join("src/input"), "broken\n")?;
    fixture.calls.store(1, Ordering::SeqCst);
    fixture.requests.lock().expect("requests lock").clear();
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Provider construction reads the process CA environment.
async fn task_verification_feedback_restart_resumes_undispatched_exact_feedback() -> Result<()> {
    let _environment_guard = crate::test_env::lock();
    let CaseResult {
        fixture, session, ..
    } = run_feedback_case(FeedbackMode::Repair, "broken\n", true).await?;
    drop(session);
    keep_feedback_crash_prefix(&fixture, false)?;
    let (execution, control) = fixture.prepare("feedback-restart").await?.into_parts();
    let output = Box::pin(execution.execute(
        &mut RecordingApplicationRunEvents::default(),
        &mut AutoApproveHandler,
    ))
    .await?;
    drop(control);
    assert_eq!(output.task_status, TaskRunStatus::Completed);
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        3,
        "resume dispatches repair+write then final; no replayed first final"
    );
    let session = fixture.session()?;
    assert_eq!(
        receipt_statuses(&session),
        [ReceiptStatus::Failed, ReceiptStatus::Succeeded]
    );
    let task = session.task_state_projection();
    assert_eq!(task.tasks[&fixture.task].direct_execution_attempts.len(), 1);
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Provider construction reads the process CA environment.
async fn task_verification_feedback_restart_rejects_changed_source_or_uncertain_dispatch()
-> Result<()> {
    let _environment_guard = crate::test_env::lock();
    for dispatched in [false, true] {
        let CaseResult {
            fixture, session, ..
        } = run_feedback_case(FeedbackMode::Repair, "broken\n", true).await?;
        drop(session);
        keep_feedback_crash_prefix(&fixture, dispatched)?;
        if !dispatched {
            std::fs::write(fixture.root.join("src/input"), "external change\n")?;
        }
        let result = fixture.prepare("feedback-restart-stale").await;
        if let Ok(prepared) = result {
            let (execution, control) = prepared.into_parts();
            let output = Box::pin(execution.execute(
                &mut RecordingApplicationRunEvents::default(),
                &mut AutoApproveHandler,
            ))
            .await;
            if let Ok(output) = output {
                assert_ne!(output.task_status, TaskRunStatus::Completed);
            }
            drop(control);
        }
        assert_eq!(
            fixture.calls.load(Ordering::SeqCst),
            1,
            "stale source and possible dispatch cannot silently issue another provider call"
        );
        assert_ne!(
            std::fs::read_to_string(fixture.root.join("src/input"))?,
            "valid\n"
        );
        assert!(
            receipt_statuses(&fixture.session()?)
                .iter()
                .all(|status| *status == ReceiptStatus::Failed)
        );
    }
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Provider construction reads the process CA environment.
async fn task_verification_feedback_cancellation_joins_real_check_without_completion() -> Result<()>
{
    let _environment_guard = crate::test_env::lock();
    let fixture = FeedbackFixture::new(FeedbackMode::Final, "broken\n", Some("from pathlib import Path; import time; Path('.check-started').write_text('started'); time.sleep(30)")).await?;
    let (execution, control) = fixture.prepare("feedback-cancel").await?.into_parts();
    let mut events = RecordingApplicationRunEvents::default();
    let mut approvals = AutoApproveHandler;
    let mut running = Box::pin(execution.execute(&mut events, &mut approvals));
    let marker = fixture.root.join(".check-started");
    let observed = async {
        loop {
            if tokio::fs::try_exists(&marker).await? {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    tokio::select! {
        result = &mut running => panic!("check finished before cancellation: {result:?}"),
        result = tokio::time::timeout(std::time::Duration::from_secs(5), observed) => { result??; }
    }
    let ticket = control.request_cancellation("cancel actual verification child", None, || {})?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), &mut running).await?;
    assert!(result.is_err(), "cancellation must win the final boundary");
    drop(running);
    assert_eq!(
        control
            .finalize_cancellation(ticket, true, &mut events)
            .await?,
        sigil_kernel::RunCancellationTerminalOutcome::Cancelled
    );
    drop(control);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    let session = fixture.session()?;
    assert_eq!(
        session.task_state_projection().tasks[&fixture.task].status,
        TaskRunStatus::Cancelled
    );
    assert!(!receipt_statuses(&session).contains(&ReceiptStatus::Succeeded));
    assert!(!session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskVerificationFeedbackV1(_))
    )));
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Provider construction reads the process CA environment.
async fn task_verification_feedback_ablation_decision_round_preserves_outcome() -> Result<()> {
    let _environment_guard = crate::test_env::lock();
    let result = run_feedback_case(FeedbackMode::Adaptive, "broken\n", true).await?;
    assert_eq!(result.status, TaskRunStatus::Completed);
    assert_eq!(result.source, "valid\n");
    assert_eq!(result.outside, "outside sentinel");
    assert_eq!(
        receipt_statuses(&result.session),
        [ReceiptStatus::Failed, ReceiptStatus::Succeeded]
    );
    println!(
        "A5 decision-round ablation: provider_calls={}, failed_checks=1, passed_checks=1, terminal=completed",
        result.calls
    );
    Ok(())
}

/// An explicit experiment starts from one controlled candidate final. Every subsequent decision
/// and tool argument comes from the real configured provider, with its actual protocol profile.
struct FirstFinalThenLiveBuilder {
    calls: Arc<AtomicUsize>,
}
struct FirstFinalThenLiveProvider {
    inner: Box<dyn Provider>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl TaskRoleProviderBuilder for FirstFinalThenLiveBuilder {
    async fn build(&self, config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        Ok(Box::new(FirstFinalThenLiveProvider {
            inner: crate::build_role_provider_async(config, role).await?,
            calls: self.calls.clone(),
        }))
    }
}

#[async_trait]
impl Provider for FirstFinalThenLiveProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }
    fn context_capabilities(&self, model: &str) -> sigil_kernel::ProviderContextCapabilities {
        self.inner.context_capabilities(model)
    }
    fn default_max_output_tokens(&self, model: &str) -> Option<u32> {
        self.inner.default_max_output_tokens(model)
    }
    fn maximum_output_tokens(&self, model: &str) -> Option<u32> {
        self.inner.maximum_output_tokens(model)
    }
    fn usage_pricing_snapshot(&self, model: &str) -> Option<sigil_kernel::ModelPricingSnapshotV1> {
        self.inner.usage_pricing_snapshot(model)
    }
    fn observe_failure(
        &self,
        error: &anyhow::Error,
        state: sigil_kernel::ProviderWireStateV1,
    ) -> sigil_kernel::ProviderFailureObservationV1 {
        self.inner.observe_failure(error, state)
    }
    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(Box::pin(stream::iter(scripted_task_completion_chunks(
                &request,
                "Controlled candidate final; the host checks have not been run yet.",
                "controlled-final",
            ))));
        }
        self.inner.stream(request).await
    }
}

#[tokio::test]
#[ignore = "requires explicit live provider authorization and SIGIL_A5_LIVE_CONFIG under the isolated live wrapper"]
#[allow(clippy::await_holding_lock)] // Provider construction reads the process CA environment.
async fn task_verification_feedback_live_controlled_failure_repair() -> Result<()> {
    let _environment_guard = crate::test_env::lock();
    let config = std::env::var_os("SIGIL_A5_LIVE_CONFIG")
        .map(std::path::PathBuf::from)
        .expect("explicit experiment config path");
    let fixture = FeedbackFixture::new_with_live_config(
        FeedbackMode::Final,
        "broken\n",
        Some(CHECK_SOURCE),
        Some(&config),
    )
    .await?;
    let started = std::time::Instant::now();
    let (execution, control) = fixture
        .prepare("feedback-live-controlled")
        .await?
        .into_parts();
    let output = Box::pin(execution.execute(
        &mut RecordingApplicationRunEvents::default(),
        &mut AutoApproveHandler,
    ))
    .await?;
    drop(control);
    let session = fixture.session()?;
    let statuses = receipt_statuses(&session);
    println!(
        "A5_LIVE {}",
        serde_json::json!({"scenario":"controlled_first_final_real_repair", "task_status":output.task_status, "real_provider_calls":fixture.calls.load(Ordering::SeqCst).saturating_sub(1), "wall_time_ms": started.elapsed().as_millis(), "receipt_statuses":statuses, "usage":session.try_usage_stats_from_durable()?})
    );
    assert_eq!(output.task_status, TaskRunStatus::Completed);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("src/input"))?,
        "valid\n"
    );
    assert_eq!(statuses, [ReceiptStatus::Failed, ReceiptStatus::Succeeded]);
    assert!(session.entries().iter().any(|entry| matches!(entry, SessionLogEntry::Control(ControlEntry::TaskVerificationFeedbackV1(feedback)) if feedback.status == sigil_kernel::TaskVerificationFeedbackStatusV1::Repair)));
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Provider construction reads the process CA environment.
async fn task_verification_feedback_multiple_checks_can_continue_past_three_repairs() -> Result<()>
{
    let _environment_guard = crate::test_env::lock();
    let fixture = FeedbackFixture::new(FeedbackMode::MultipleChecks, "0\n", None).await?;
    let mut session = fixture.session()?;
    let scope = EvidenceScope::Task(fixture.task.as_str().to_owned());
    let mut checks = Vec::new();
    for minimum in 1..=4 {
        let id = format!("minimum-{minimum}");
        let trusted = CandidateCheck {
            source: CheckDiscoverySource::UserExplicitConfig,
            command: CheckCommand { command:"python3".to_owned(), args:vec!["-c".to_owned(), format!("from pathlib import Path; import sys; ok = int(Path('src/input').read_text().strip()) >= {minimum}; print('src/input:1: requires at least {minimum}', file=sys.stderr); sys.exit(0 if ok else 1)")], cwd:None },
            source_event_id: id.clone(), workspace_trust_snapshot_id:"user-config".to_owned(),
        }.promote(id.clone(), "bounded-source", ToolEffect::ReadOnly, CheckPromotion::ExplicitUserConfig { config_event_id:id.clone() })?;
        session.append_control(ControlEntry::CheckSpecRecorded(
            CheckSpecRecordedEntry::new(scope.clone(), trusted.clone(), id),
        ))?;
        checks.push(trusted.check_spec);
    }
    let mut policy = VerificationPolicy::no_checks_required("bounded-source");
    policy.required_checks = checks;
    policy.verification_scope.include = vec!["src/**".to_owned()];
    policy.verification_scope.tracked_files_only = false;
    policy.completion_criteria = CompletionCriteria::AllRequiredChecks;
    policy.allow_unverified_completion = false;
    policy.auto_run = VerificationAutoRunPolicy::TrustedOnly;
    session.append_control(ControlEntry::VerificationPolicyChanged(
        VerificationPolicyChangedEntry::new(scope, policy, "four-declared-checks")?,
    ))?;
    drop(session);
    let (execution, control) = fixture.prepare("feedback-four-checks").await?.into_parts();
    let output = Box::pin(execution.execute(
        &mut RecordingApplicationRunEvents::default(),
        &mut AutoApproveHandler,
    ))
    .await?;
    drop(control);
    assert_eq!(output.task_status, TaskRunStatus::Completed);
    if fixture.calls.load(Ordering::SeqCst) != 9 {
        let observed = fixture.session()?.verification_state_projection();
        println!(
            "A5_MULTI_CHECK_DIAGNOSTIC {}",
            serde_json::json!({
                "calls": fixture.calls.load(Ordering::SeqCst),
                "source": std::fs::read_to_string(fixture.root.join("src/input"))?,
                "policies": observed.policies.values().collect::<Vec<_>>(),
                "receipts": observed.receipts.values().collect::<Vec<_>>()
            })
        );
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 9);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("src/input"))?,
        "4\n"
    );
    let session = fixture.session()?;
    assert_eq!(
        receipt_statuses(&session)
            .iter()
            .filter(|status| **status == ReceiptStatus::Failed)
            .count(),
        4
    );
    assert_eq!(session.entries().iter().filter(|entry| matches!(entry, SessionLogEntry::Control(ControlEntry::TaskVerificationFeedbackV1(feedback)) if feedback.status == sigil_kernel::TaskVerificationFeedbackStatusV1::Repair)).count(), 4);
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn task_verification_feedback_diagnostics_follow_real_application_root_run() -> Result<()> {
    use sigil_kernel::run_diagnostics::{RunTimingPhase, run_timing_key, run_timing_snapshot};
    let _environment_guard = crate::test_env::lock();
    let fixture =
        FeedbackFixture::new(FeedbackMode::Repair, "broken\n", Some(CHECK_SOURCE)).await?;
    let run_id = format!("feedback-diagnostics-{}", uuid::Uuid::new_v4());
    let before = run_timing_snapshot();
    assert!(before.available);
    let first_sequence = before
        .observations
        .iter()
        .map(|entry| entry.sequence)
        .max()
        .unwrap_or(0);
    let (execution, control) = fixture.prepare(&run_id).await?.into_parts();
    let output = Box::pin(execution.execute(
        &mut RecordingApplicationRunEvents::default(),
        &mut AutoApproveHandler,
    ))
    .await?;
    drop(control);
    assert_eq!(output.task_status, TaskRunStatus::Completed);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 3);
    let after = run_timing_snapshot();
    assert!(after.available);
    assert_eq!(before.process_instance, after.process_instance);
    let last_sequence = after
        .observations
        .iter()
        .map(|entry| entry.sequence)
        .max()
        .unwrap_or(0);
    let observed = after
        .observations
        .iter()
        .filter(|entry| {
            entry.sequence > first_sequence
                && entry.sequence <= last_sequence
                && entry.phase == RunTimingPhase::ProviderDispatch
        })
        .collect::<Vec<_>>();
    let root_key = run_timing_key(&run_id);
    assert!(
        after
            .observations
            .iter()
            .any(|entry| entry.sequence > first_sequence
                && entry.sequence <= last_sequence
                && entry.run_key == root_key
                && entry.phase == RunTimingPhase::ToolExecution),
        "actual Task repair tool must retain the same application root diagnostic ID"
    );
    assert_eq!(
        observed
            .iter()
            .filter(|entry| entry.run_key == root_key)
            .count(),
        3,
        "each actual Task model request must retain the application root diagnostic ID"
    );
    let session = fixture.session()?;
    let task = session.task_state_projection();
    for attempt_id in task.tasks[&fixture.task].direct_execution_attempts.keys() {
        let attempt_key = run_timing_key(&sigil_kernel::task_direct_execution_logical_run_id(
            attempt_id,
        ));
        assert!(
            !observed.iter().any(|entry| entry.run_key == attempt_key),
            "Task attempts are execution identities, not the user's diagnostic run root"
        );
    }
    Ok(())
}
