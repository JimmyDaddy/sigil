use super::*;

struct ManagedPlanRecoveryFixture {
    driver: Arc<HttpProductionRunDriver>,
    registry: Arc<HttpSessionRunRegistry>,
    provider_calls: Arc<AtomicUsize>,
    provider_response_started: Arc<tokio::sync::Semaphore>,
    provider_response_release: Arc<tokio::sync::Semaphore>,
    provider_server: tokio::task::JoinHandle<()>,
}

async fn managed_plan_recovery_fixture(
    temp: &tempfile::TempDir,
    pause_provider_response: bool,
) -> ManagedPlanRecoveryFixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local provider should bind");
    let address = listener.local_addr().expect("provider address");
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let provider_response_started = Arc::new(tokio::sync::Semaphore::new(0));
    let provider_response_release = Arc::new(tokio::sync::Semaphore::new(usize::from(
        !pause_provider_response,
    )));
    let calls = Arc::clone(&provider_calls);
    let started = Arc::clone(&provider_response_started);
    let release = Arc::clone(&provider_response_release);
    let provider_server = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut request = Vec::new();
            let mut buffer = [0_u8; 8192];
            loop {
                let read = socket
                    .read(&mut buffer)
                    .await
                    .expect("provider request should read");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if let Some(headers_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&request[..headers_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .expect("provider request should have a body length");
                    if request.len() >= headers_end + 4 + content_length {
                        break;
                    }
                }
            }
            let ordinal = calls.fetch_add(1, Ordering::SeqCst);
            if ordinal == 0 {
                started.add_permits(1);
                release
                    .acquire()
                    .await
                    .expect("provider response release")
                    .forget();
            }
            let delta = serde_json::json!({"tool_calls": [{
                "index": 0,
                "id": format!("managed-recovered-draft-{ordinal}"),
                "type": "function",
                "function": {
                    "name": sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME,
                    "arguments": serde_json::json!({
                        "schema_version": 1,
                        "outcome": "draft",
                        "content": "# Recovered managed Plan\n\n1. Preserve the public contract."
                    }).to_string()
                }
            }]});
            let body = format!(
                "data: {}\n\ndata: [DONE]\n\n",
                serde_json::json!({"choices": [{
                    "delta": delta,
                    "finish_reason": "tool_calls"
                }]})
            );
            socket.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ).as_bytes()).await.expect("provider response should write");
        }
    });
    let config_path = temp.path().join("sigil.toml");
    write_production_test_config(&config_path, ".");
    let config = std::fs::read_to_string(&config_path)
        .expect("fixture config should read")
        .replace("http://127.0.0.1:1", &format!("http://{address}"));
    std::fs::write(&config_path, config).expect("provider route should update");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir(&sessions).expect("session directory should create");
    let lifecycle = LocalSessionLifecycleService::new(
        "managed-plan-recovery",
        &sessions,
        temp.path().join("exports"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(
        32,
        Arc::new(
            HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 32)
                .expect("protocol journal should open"),
        ),
    ));
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(&config_path, temp.path())
                .with_session_lifecycle(lifecycle),
            Arc::new(
                HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 32)
                    .expect("disclosure journal should open"),
            ),
            event_bus,
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should initialize"),
    );
    let registry = driver
        .build_registry(Arc::new(
            HttpDurableCommandStore::open(temp.path().join("commands.json"), 32)
                .expect("command store should open"),
        ))
        .expect("production registry should attach");
    ManagedPlanRecoveryFixture {
        driver,
        registry,
        provider_calls,
        provider_response_started,
        provider_response_release,
        provider_server,
    }
}

fn managed_plan_recovery_open_request(
    driver: &HttpProductionRunDriver,
    session: &HttpSessionSnapshot,
) -> HttpSessionOpenRequest {
    let catalog = driver
        .session_lifecycle()
        .expect("production lifecycle should be composed")
        .catalog()
        .expect("managed session catalog should read");
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.session_id.as_deref() == Some(&session.durable_session_scope_id))
        .expect("the exact parent should appear in the managed catalog");
    HttpSessionOpenRequest {
        session_ref: entry
            .session_ref
            .as_path()
            .to_str()
            .expect("UTF-8 catalog ref")
            .to_owned(),
        session_id: session.durable_session_scope_id.clone(),
        label: None,
        recovery_binding: None,
    }
}

fn managed_plan_revision_guidance(
    fixture: &ManagedPlanRecoveryFixture,
    temp: &tempfile::TempDir,
    session: &HttpSessionSnapshot,
) -> HttpUserInputRequest {
    let review_id = seed_revision_session(temp, session);
    fixture
        .registry
        .plan_decision_command(
            &session.id,
            HttpCommandEnvelope::new(
                "managed-revision-guidance",
                "client-1",
                &session.id,
                HttpPlanDecisionRequest {
                    plan_id: sigil_kernel::plan_review_plan_id_for_attempt(
                        &review_id,
                        &sigil_kernel::plan_review_attempt_id_for_review(&review_id),
                    )
                    .as_str()
                    .to_owned(),
                    expected_plan_hash: format!("sha256:{}", "d".repeat(64)),
                    expected_candidate_hash: None,
                    action: HttpPlanDecisionAction::Revise,
                    permission_grant: None,
                },
            ),
        )
        .expect("Revise should create durable guidance")
        .user_input_request
        .expect("revision guidance should be returned")
}

async fn assert_production_reopen_recovers_managed_plan_draft(retry_attachment: bool) {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let fixture = managed_plan_recovery_fixture(&temp, false).await;
    let session = fixture
        .registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("parent session should create");
    let mut parent = sigil_kernel::Session::load_from_store_for_control(
        JsonlSessionStore::new(&session.session_log_path).expect("parent store"),
    )
    .expect("parent should load without startup interruption");
    let request = sigil_runtime::PlanReviewCoordinator::prepare_explicit_plan_review(
        &mut parent,
        "Prepare the bounded managed recovery Plan.",
        "managed-plan-recovery-run",
        None,
        current_unix_time_ms(),
    )
    .expect("explicit Plan should prepare");
    let root_config = RootConfig::load(&temp.path().join("sigil.toml"))
        .expect("root config")
        .with_effective_composition()
        .expect("effective composition");
    let provider = sigil_runtime::build_provider_for_model_ref_async(
        &root_config,
        &parent
            .resolved_model_route()
            .expect("selected route")
            .model_ref,
    )
    .await
    .expect("real local provider should assemble");
    let agent =
        sigil_runtime::configured_agent(&root_config, provider, sigil_kernel::ToolRegistry::new())
            .expect("Plan agent should assemble");
    let mut options = sigil_runtime::build_run_options(
        &root_config,
        temp.path().to_path_buf(),
        sigil_kernel::InteractionMode::Interactive,
        None,
    );
    options.memory_config = sigil_kernel::MemoryConfig::with_enabled(false);
    let provisioner = fixture
        .driver
        .services
        .authority_composition()
        .expect("production authority should be composed")
        .plan_review_child_resource_provisioner();
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        sigil_runtime::PlanReviewCoordinator::run_plan_review_with_resource_provisioner(
            &mut parent,
            &request,
            &agent,
            options,
            sigil_kernel::ToolRegistry::new(),
            &mut sigil_kernel::NoopEventHandler,
            &mut sigil_kernel::AutoApproveHandler,
            sigil_kernel::RunCancellationOwner::new().handle(),
            provisioner,
        ),
    )
    .await
    .expect("the local provider should complete its bounded draft")
    .expect("the actual managed child should finish its typed draft");
    let sigil_runtime::PlanReviewRunOutcome::DraftReady { draft } = outcome else {
        panic!("the real provider must complete a typed draft before the crash boundary");
    };
    assert_eq!(fixture.provider_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        sigil_kernel::PlanReviewProjection::from_entries(parent.entries())
            .latest_attempt(&request.plan_review_id)
            .expect("active parent attempt")
            .status,
        sigil_kernel::PlanReviewAttemptStatus::Started,
    );
    assert!(
        !parent
            .plan_artifact_projection()
            .plans
            .contains_key(&request.plan_id)
    );
    // Stop at the actual caller boundary: the managed child finished, but its owner has not
    // committed the parent outcome. No JSONL truncation or handcrafted child result is used.
    drop(parent);
    fixture.provider_server.abort();
    fixture
        .driver
        .purge_session_local_state(&session.durable_session_scope_id);
    let mut open_request = managed_plan_recovery_open_request(&fixture.driver, &session);
    if retry_attachment {
        let external = sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            Path::new(&session.session_log_path),
        ).expect("external controller should hold the exact attachment");
        let before = std::fs::read(&session.session_log_path).expect("crash prefix should read");
        let busy = fixture
            .registry
            .open_session(open_request.clone())
            .expect("busy parent should open as a read handle");
        let recovery = busy
            .route_recovery
            .expect("busy session should retain retry binding");
        assert_eq!(
            recovery.code,
            HttpSessionRouteRecoveryCode::SessionAlreadyActive
        );
        assert_eq!(
            std::fs::read(&session.session_log_path).expect("busy parent should read"),
            before
        );
        open_request.recovery_binding = Some(recovery.recovery_binding);
        drop(external);
    }
    let opened = fixture
        .registry
        .open_session(open_request.clone())
        .expect("HTTP activation should recover the exact managed draft before startup");
    assert!(opened.route_recovery.is_none());
    let restored = sigil_kernel::Session::load_from_store_for_control(
        JsonlSessionStore::new(&session.session_log_path).expect("restored parent store"),
    )
    .expect("recovered parent should read");
    assert_eq!(
        restored
            .plan_artifact_projection()
            .plans
            .get(&request.plan_id),
        Some(draft.as_ref())
    );
    assert_eq!(
        sigil_kernel::PlanReviewProjection::from_entries(restored.entries())
            .latest_attempt(&request.plan_review_id)
            .expect("recovered attempt")
            .status,
        sigil_kernel::PlanReviewAttemptStatus::DraftReady,
    );
    assert!(restored.entries().iter().all(|entry| !matches!(entry,
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
            if attempt.attempt_id == request.attempt_id
                && attempt.status == sigil_kernel::PlanReviewAttemptStatus::Interrupted
    )));
    let display = fixture
        .registry
        .conversation_display_page(&opened.id, None, 50)
        .expect("HTTP should expose the recovered Plan");
    assert_eq!(
        display.plan_review.expect("recovered Plan card").status,
        crate::HttpPlanReviewStatus::DraftReady
    );
    let before = std::fs::read(&session.session_log_path).expect("recovered parent bytes");
    open_request.recovery_binding = None;
    fixture
        .registry
        .open_session(open_request)
        .expect("repeat open should be idempotent");
    assert_eq!(
        std::fs::read(&session.session_log_path).expect("reopened parent bytes"),
        before
    );
    assert_eq!(
        fixture.provider_calls.load(Ordering::SeqCst),
        1,
        "recovery must not call the provider"
    );
}

#[tokio::test]
async fn production_session_reopen_recovers_managed_plan_draft_before_startup() {
    assert_production_reopen_recovers_managed_plan_draft(false).await;
}

#[tokio::test]
async fn production_session_attach_retry_recovers_managed_plan_draft_before_startup() {
    assert_production_reopen_recovers_managed_plan_draft(true).await;
}

#[tokio::test]
async fn production_session_reopen_preserves_active_plan_and_parent_log() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let fixture = managed_plan_recovery_fixture(&temp, true).await;
    let session = fixture
        .registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("parent session should create");
    let guidance = managed_plan_revision_guidance(&fixture, &temp, &session);
    let registry = Arc::clone(&fixture.registry);
    let session_id = session.id.clone();
    let receipt = tokio::task::spawn_blocking(move || {
        registry.user_input_decision_command(
            &session_id,
            guidance.identity.request_id.as_str(),
            HttpCommandEnvelope::new(
                "managed-active-guidance",
                "client-1",
                &session_id,
                HttpUserInputDecisionRequest {
                    generation: guidance.identity.generation,
                    expected_request_hash: guidance.request_hash.clone(),
                    decision: production_revision_guidance_answer(),
                    permission_mode: None,
                },
            ),
        )
    })
    .await
    .expect("guidance caller should join")
    .expect("real HTTP revision should start");
    let run_id = receipt.continuation_run_id.expect("revision run identity");
    tokio::time::timeout(
        Duration::from_secs(15),
        fixture.provider_response_started.acquire(),
    )
    .await
    .expect("the revision should reach its ordinary provider response")
    .expect("provider response signal")
    .forget();
    let before = std::fs::read(&session.session_log_path).expect("live parent bytes");
    let open_request = managed_plan_recovery_open_request(&fixture.driver, &session);
    let opened = fixture
        .registry
        .open_session(open_request)
        .expect("the running Plan should remain readable");
    assert_eq!(opened.id, session.id);
    assert_eq!(
        std::fs::read(&session.session_log_path).expect("live parent bytes after open"),
        before
    );
    assert_eq!(
        fixture
            .registry
            .get_run(&run_id)
            .expect("live run remains registered")
            .status,
        HttpRunStatus::Running
    );
    assert!(
        fixture
            .driver
            .active_runs
            .lock()
            .expect("active-run state")
            .contains_key(&run_id)
    );
    fixture.provider_response_release.add_permits(1);
    let driver = Arc::clone(&fixture.driver);
    tokio::task::spawn_blocking(move || driver.wait_for_idle(Duration::from_secs(15)))
        .await
        .expect("idle caller should join")
        .expect("the original worker must finish");
    assert_eq!(
        fixture
            .registry
            .get_run(&run_id)
            .expect("completed run")
            .status,
        HttpRunStatus::Finished
    );
    assert_eq!(fixture.provider_calls.load(Ordering::SeqCst), 1);
    fixture.provider_server.abort();
}

#[tokio::test]
async fn production_session_reopen_preserves_registered_queued_plan_and_parent_log() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let fixture = managed_plan_recovery_fixture(&temp, false).await;
    let session = fixture
        .registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("parent session should create");
    let guidance = managed_plan_revision_guidance(&fixture, &temp, &session);
    let mutation = fixture
        .registry
        .reserve_durable_session_mutation(&session.durable_session_scope_id)
        .expect("guidance acceptance should reserve its frontier");
    let request = accept_production_revision_guidance(&temp, &session, &guidance);
    let run_id = request.child_logical_run_id();
    fixture
        .registry
        .register_supervised_session_run(
            &session.id,
            &run_id,
            HttpPermissionMode::ReadOnly,
            "queued managed Plan",
        )
        .expect("accepted revision should register before worker dispatch");
    drop(mutation);
    assert!(
        !fixture
            .driver
            .active_runs
            .lock()
            .expect("active-run state")
            .contains_key(&run_id)
    );
    let before = std::fs::read(&session.session_log_path).expect("queued parent bytes");
    let opened = fixture
        .registry
        .open_session(managed_plan_recovery_open_request(
            &fixture.driver,
            &session,
        ))
        .expect("the queued Plan should remain readable");
    assert_eq!(opened.id, session.id);
    assert_eq!(
        std::fs::read(&session.session_log_path).expect("queued parent bytes after open"),
        before
    );
    assert_eq!(
        fixture
            .registry
            .get_run(&run_id)
            .expect("queued owner remains")
            .status,
        HttpRunStatus::Running
    );
    assert_eq!(
        fixture.provider_calls.load(Ordering::SeqCst),
        0,
        "open must not dispatch the queued Plan"
    );
    fixture
        .registry
        .rollback_supervised_session_run_registration(&session.id, &run_id);
    fixture.provider_server.abort();
}
