use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use anyhow::Result;
use sigil_kernel::{
    ControlEntry, ImageAttachment, ImageAttachmentResolver, JsonlSessionStore,
    RunCancellationHandle, RunCancellationTarget, RunCancellationTerminalOutcome, RunEffectClass,
    RunEffectKind, Session,
};

use super::super::{
    WorkerMessage,
    elicitation_bridge::ChannelMcpElicitationHandler,
    terminal_lifecycle_bridge::ChannelTerminalLifecycleRouter,
    worker_event::WorkerWakeCoalescer,
    worker_loop::{
        ActiveRun, ActiveRunStopDisposition, WorkerLoopState, cancel_active_run,
        prepare_run_cancellation,
    },
};
use super::common::test_root_config;

struct FixtureImageResolver;

impl ImageAttachmentResolver for FixtureImageResolver {
    fn resolve(&self, _attachment: &ImageAttachment) -> Result<Vec<u8>> {
        anyhow::bail!("the cancellation fixture does not read images")
    }
}

struct CancellationFixture {
    root_config: sigil_kernel::RootConfig,
    state: WorkerLoopState,
    runtime: tokio::runtime::Runtime,
    message_tx: mpsc::Sender<WorkerMessage>,
    message_rx: mpsc::Receiver<WorkerMessage>,
    handler: Arc<ChannelMcpElicitationHandler>,
}

impl CancellationFixture {
    fn new(root: &std::path::Path) -> Result<Self> {
        let path = root.join("session.jsonl");
        let mut session =
            Session::load_from_store("planned", "planned-model", JsonlSessionStore::new(&path)?)?;
        sigil_runtime::attach_session_url_capability_store(&mut session)?;
        session.try_attach_image_attachment_resolver(Arc::new(FixtureImageResolver))?;
        let (event_tx, _event_rx) = mpsc::channel();
        let state = WorkerLoopState::new_with_optional_attachment(
            path,
            Some(session),
            None,
            None,
            Default::default(),
            event_tx.clone(),
            WorkerWakeCoalescer::new(event_tx.clone(), None),
            ChannelTerminalLifecycleRouter::new(event_tx),
            None,
            None,
            None,
            None,
        );
        let (message_tx, message_rx) = mpsc::channel();
        Ok(Self {
            root_config: test_root_config(root, "planned", "planned-model"),
            state,
            runtime: tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?,
            handler: Arc::new(ChannelMcpElicitationHandler::new(message_tx.clone())),
            message_tx,
            message_rx,
        })
    }

    fn active(
        &mut self,
        fail_cleanup: bool,
        panic: bool,
    ) -> (
        ActiveRun,
        RunCancellationHandle,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let session = self
            .state
            .session
            .current
            .as_ref()
            .expect("fixture session");
        let (owner, recorder, cancellation, root_guard) =
            prepare_run_cancellation(session).expect("prepare real cancellation owner");
        self.state.stop_control.bind(&owner);
        let task_guard = cancellation
            .register_task()
            .expect("register cleanup child");
        let effect = cancellation
            .begin_effect(RunEffectClass::Cleanup, RunEffectKind::Process)
            .expect("register actual cleanup effect");
        let (release, released) = tokio::sync::oneshot::channel();
        let cleanup_cancellation = cancellation.clone();
        self.state.run.retired.push(self.runtime.spawn(async move {
            cleanup_cancellation.cancelled().await;
            let _ = released.await;
            if fail_cleanup {
                cleanup_cancellation.mark_cleanup_incomplete();
            }
            drop(effect);
            drop(task_guard);
        }));
        let root_cancellation = cancellation.clone();
        let handle = self.runtime.spawn(async move {
            let _root_guard = root_guard;
            root_cancellation.cancelled().await;
            assert!(!panic, "controlled root cleanup panic");
        });
        let (approval_tx, _approval_rx) = mpsc::channel();
        (
            ActiveRun {
                run_id: 1,
                public_run_id: None,
                handle,
                approval_tx,
                elicitation_audit_buffer: Arc::new(Mutex::new(Vec::<ControlEntry>::new())),
                cancellation_owner: owner,
                cancellation_recorder: recorder,
                cancellation_target: RunCancellationTarget::Run,
                revision_terminal_run_id: None,
                url_capability_registrar: session.user_url_capability_registrar(),
                image_attachment_resolver: session.image_attachment_resolver(),
            },
            cancellation,
            release,
        )
    }

    fn settle(&mut self, active: ActiveRun) {
        cancel_active_run(
            active,
            &self.runtime,
            &self.root_config,
            &mut self.state,
            &self.message_tx,
            &self.handler,
            ActiveRunStopDisposition::Cancel,
            "fixture cancel",
        );
        self.runtime
            .block_on(
                self.state
                    .refresh
                    .provider_status_tasks
                    .shutdown_until(std::time::Instant::now() + Duration::from_secs(1)),
            )
            .expect("join independent provider observation owner");
        for handle in self.state.run.retired.drain(..) {
            self.runtime
                .block_on(handle)
                .expect("join independently owned cleanup task");
        }
    }
}

#[derive(Debug)]
enum DurableRunCancellationRecord {
    Requested(sigil_kernel::RunCancellationRequestedEntry),
    Finalized(sigil_kernel::RunCancellationFinalizedEntry),
}

fn records(path: &std::path::Path) -> Result<Vec<DurableRunCancellationRecord>> {
    JsonlSessionStore::read_event_records(path)?
        .into_iter()
        .filter_map(|record| {
            let event = record.stored_event();
            matches!(
                event
                    .payload
                    .get("record")
                    .and_then(serde_json::Value::as_str),
                Some("requested" | "finalized")
            )
            .then(|| {
                let payload = event.payload.clone();
                match payload.get("record").and_then(serde_json::Value::as_str) {
                    Some("requested") => serde_json::from_value(payload)
                        .map(DurableRunCancellationRecord::Requested)
                        .map_err(Into::into),
                    Some("finalized") => serde_json::from_value(payload)
                        .map(DurableRunCancellationRecord::Finalized)
                        .map_err(Into::into),
                    _ => unreachable!("only cancellation records are selected"),
                }
            })
        })
        .collect()
}

#[test]
fn slow_foreground_cancellation_keeps_original_owner_until_one_true_finalization() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut fixture = CancellationFixture::new(temp.path())?;
    let path = fixture.state.session.log_path.clone();
    let stop = fixture.state.stop_control.clone();
    // A real independent observation request stays pending while ordinary cancellation waits.
    // Once closing arrives it must be stopped before the root cleanup barrier is released.
    let listener = fixture
        .runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))?;
    let address = listener.local_addr()?;
    let (request_seen, request_rx) = mpsc::channel();
    let (connection_closed, closed_rx) = mpsc::channel();
    let server = fixture.runtime.spawn(async move {
        use tokio::io::AsyncReadExt;
        let (mut socket, _) =
            tokio::time::timeout(Duration::from_secs(5), listener.accept()).await??;
        let mut headers = Vec::new();
        let mut byte = [0_u8; 1];
        while !headers.ends_with(b"\r\n\r\n") {
            tokio::time::timeout(Duration::from_secs(5), socket.read_exact(&mut byte)).await??;
            headers.push(byte[0]);
            anyhow::ensure!(
                headers.len() < 8192,
                "fixture request headers must be bounded"
            );
        }
        request_seen.send(())?;
        let bytes = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut byte)).await??;
        connection_closed.send(bytes == 0)?;
        anyhow::Ok(())
    });
    let (provider_result, _provider_rx) = mpsc::channel();
    fixture.state.refresh.provider_status_tasks.refresh_models(
        &fixture.runtime,
        1,
        sigil_runtime::ProviderStatusConfig {
            api_key: Some("fixture-only".to_owned()),
            base_url: format!("http://{address}"),
            request_timeout_secs: 30,
        },
        provider_result,
    );
    request_rx.recv_timeout(Duration::from_secs(5))?;
    let (active, cancellation, release) = fixture.active(false, false);
    let (observed_tx, observed_rx) = mpsc::channel();
    let input = std::mem::replace(&mut fixture.message_rx, observed_rx);
    drop(observed_tx);
    let worker = std::thread::spawn(move || {
        fixture.settle(active);
        fixture
    });
    let mut requested = false;
    loop {
        match input.recv_timeout(Duration::from_secs(5))? {
            WorkerMessage::RunCancellationRequested => requested = true,
            WorkerMessage::Notice(text) if text.contains("still cleaning up") => break,
            WorkerMessage::RunCancelled { .. } | WorkerMessage::RunInterrupted { .. } => {
                panic!("terminal published before the cleanup barrier released")
            }
            _ => {}
        }
    }
    assert!(requested);
    assert!(!worker.is_finished());
    assert_eq!(cancellation.active_effects(), 1);
    assert_eq!(
        cancellation.active_tasks(),
        1,
        "the root has returned but its child is still owned"
    );
    assert!(
        cancellation.cleanup_complete(),
        "a missed responsiveness target is not a cleanup failure"
    );
    let before = records(&path)?;
    assert!(matches!(
        before.as_slice(),
        [DurableRunCancellationRecord::Requested(_)]
    ));
    // The same cancellation owner must also survive an exit request received while ordinary
    // cancellation is pending; no second request or premature terminal is introduced.
    stop.reserve(true);
    let independent_stopped_before_root_release = closed_rx.recv_timeout(Duration::from_secs(2));
    release.send(()).expect("release real cleanup barrier");
    let fixture = worker.join().expect("join settlement worker");
    fixture
        .runtime
        .block_on(server)
        .expect("join provider fixture server")?;
    assert!(
        independent_stopped_before_root_release?,
        "closing must stop the real provider request while root cleanup is still pending"
    );
    let after = records(&path)?;
    let [
        DurableRunCancellationRecord::Requested(request),
        DurableRunCancellationRecord::Finalized(finalized),
    ] = after.as_slice()
    else {
        panic!("one request must be followed by exactly one finalization: {after:?}");
    };
    assert_eq!(request.run_scope_id, cancellation.scope_id());
    assert_eq!(finalized.request_id, request.request_id);
    assert_eq!(finalized.run_scope_id, request.run_scope_id);
    assert!(finalized.finalized_at_ms >= request.quiescence_deadline_ms);
    assert_eq!(
        finalized.outcome,
        RunCancellationTerminalOutcome::Cancelled,
        "finalized cancellation record: {finalized:?}"
    );
    assert!(finalized.cleanup_complete);
    assert_eq!((finalized.active_effects, finalized.active_tasks), (0, 0));
    assert!(cancellation.cleanup_complete());
    assert!(fixture.state.run.retired.is_empty());
    assert!(
        input
            .try_iter()
            .any(|message| matches!(message, WorkerMessage::RunCancelled { .. }))
    );
    Ok(())
}

#[test]
fn real_cleanup_failure_and_panic_remain_interrupted_after_all_guards_release() -> Result<()> {
    for (cleanup_failure, panic, expected_reason) in [
        (true, false, "cleanup reported a failure"),
        (false, true, "panicked during cancellation"),
    ] {
        let temp = tempfile::tempdir()?;
        let mut fixture = CancellationFixture::new(temp.path())?;
        let (active, cancellation, release) = fixture.active(cleanup_failure, panic);
        release.send(()).expect("allow actual cleanup to finish");
        fixture.settle(active);
        let after = records(&fixture.state.session.log_path)?;
        let DurableRunCancellationRecord::Finalized(finalized) =
            after.last().expect("finalized cancellation")
        else {
            panic!("missing finalization")
        };
        assert_eq!(
            finalized.outcome,
            RunCancellationTerminalOutcome::Interrupted
        );
        assert!(!finalized.cleanup_complete);
        assert_eq!((finalized.active_effects, finalized.active_tasks), (0, 0));
        assert!(finalized.reason.contains(expected_reason));
        assert!(
            !cancellation.cleanup_complete(),
            "real failure must remain latched after join"
        );
        assert!(
            fixture
                .message_rx
                .try_iter()
                .any(|message| matches!(message, WorkerMessage::RunInterrupted { .. }))
        );
    }
    Ok(())
}

#[test]
fn required_post_cancel_audit_failure_preserves_successful_physical_cleanup_record_and_error()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut fixture = CancellationFixture::new(temp.path())?;
    let (active, cancellation, release) = fixture.active(false, false);
    let audit = Arc::clone(&active.elicitation_audit_buffer);
    let _ = std::panic::catch_unwind(move || {
        let _guard = audit.lock().expect("lock fixture audit buffer");
        panic!("inject required audit failure");
    });
    release.send(()).expect("allow cleanup");
    fixture.settle(active);
    let after = records(&fixture.state.session.log_path)?;
    let [
        DurableRunCancellationRecord::Requested(_),
        DurableRunCancellationRecord::Finalized(finalized),
    ] = after.as_slice()
    else {
        panic!("one immutable cancellation result")
    };
    assert!(
        finalized.cleanup_complete,
        "the earlier physical cleanup fact is append-only"
    );
    assert_eq!(finalized.outcome, RunCancellationTerminalOutcome::Cancelled);
    assert!(
        !cancellation.cleanup_complete(),
        "necessary later audit failure must remain in the owner stop result"
    );
    let messages = fixture.message_rx.try_iter().collect::<Vec<_>>();
    assert!(
        messages
            .iter()
            .any(|message| matches!(message, WorkerMessage::RunFailed(_)))
    );
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, WorkerMessage::RunCancelled { .. }))
    );
    Ok(())
}

#[test]
fn tui_task_cancellation_keeps_ownerless_background_child_interrupted() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut fixture = CancellationFixture::new(temp.path())?;
    let task_id = sigil_kernel::TaskId::new("tui_cancel_background_task")?;
    let thread_id = sigil_kernel::AgentThreadId::new("tui_cancel_background_child")?;
    let attempt_id = sigil_kernel::AgentRunAttemptId::new("tui_cancel_background_attempt")?;
    let profile_id = sigil_kernel::AgentProfileId::new("explore")?;
    let snapshot_id = sigil_kernel::AgentProfileSnapshotId::new("tui_cancel_background_profile")?;
    let session = fixture
        .state
        .session
        .current
        .as_mut()
        .expect("fixture session");
    session.append_control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: sigil_kernel::SessionRef::new_relative("session.jsonl")?,
        objective: "stop the background task".to_owned(),
        title: None,
        status: sigil_kernel::TaskRunStatus::Running,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskDirectExecutionAdmittedV1(
        sigil_kernel::TaskDirectExecutionAdmittedV1::task_request(
            task_id.clone(),
            "stop the background task",
            1,
        ),
    ))?;
    session.append_control(ControlEntry::AgentProfileCaptured(
        sigil_kernel::AgentProfileCapturedEntry {
            snapshot: sigil_kernel::AgentProfileSnapshot {
                snapshot_id: snapshot_id.clone(),
                profile_id: profile_id.clone(),
                source: sigil_kernel::AgentProfileSource::System,
                source_hash: "sha256:profile-source".to_owned(),
                profile_hash: "sha256:profile".to_owned(),
                resolved_tool_scope_hash: "sha256:tools".to_owned(),
                resolved_permission_policy_hash: "sha256:permissions".to_owned(),
                resolved_mcp_scope_hash: "sha256:mcp".to_owned(),
                resolved_skill_hashes: Vec::new(),
                trust_state: sigil_kernel::AgentTrustState::Trusted,
            },
        },
    ))?;
    session.append_control(ControlEntry::AgentThreadStarted(
        sigil_kernel::AgentThreadStartedEntry {
            thread_id: thread_id.clone(),
            parent_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
            batch_id: None,
            batch_member_key: None,
            parent_session_ref: sigil_kernel::SessionRef::new_relative("session.jsonl")?,
            thread_session_ref: sigil_kernel::SessionRef::new_relative(
                "children/tui-cancel-child.jsonl",
            )?,
            profile_id: profile_id.clone(),
            profile_snapshot_id: snapshot_id.clone(),
            run_context: sigil_kernel::AgentRunContextSnapshot {
                profile_snapshot_id: snapshot_id,
                provider: "planned".to_owned(),
                model: "planned-model".to_owned(),
                model_ref: None,
                reasoning_effort: None,
                workspace_root: sigil_kernel::WorkspaceRootSnapshot::new(".")?,
                effective_tool_scope_hash: "sha256:tools".to_owned(),
                effective_permission_policy_hash: "sha256:permissions".to_owned(),
                effective_mcp_scope_hash: "sha256:mcp".to_owned(),
                provider_capability_hash: "sha256:provider".to_owned(),
                model_visible_agent_index_hash: None,
                budget_policy_hash: "sha256:budget".to_owned(),
                provider_background_handle_ref: None,
            },
            objective: "wait for approval".to_owned(),
            prompt_hash: "sha256:child-prompt".to_owned(),
            invocation_mode: sigil_kernel::AgentInvocationMode::Background,
            invocation_source: sigil_kernel::AgentInvocationSource::Task,
            display_name: None,
            created_at_ms: Some(2),
        },
    ))?;
    let tool_contract_fingerprint = format!("sha256:{}", "a".repeat(64));
    session.append_control(ControlEntry::AgentDelegationAdmitted(
        sigil_kernel::AgentDelegationAdmissionEntry {
            thread_id: thread_id.clone(),
            profile_id,
            invocation_mode: sigil_kernel::AgentInvocationMode::Background,
            invocation_source: sigil_kernel::AgentInvocationSource::Task,
            authority: sigil_kernel::DelegationAuthorityRecord::DirectTask {
                task_id: task_id.clone(),
            },
            objective_hash: format!("sha256:{}", "b".repeat(64)),
            tool_contract_fingerprint: tool_contract_fingerprint.clone(),
            invocation_grant: Some(sigil_kernel::AgentInvocationGrantRecord {
                grant_fingerprint: format!("sha256:{}", "c".repeat(64)),
                source: sigil_kernel::AgentInvocationGrantSource::DirectTask {
                    task_id: task_id.clone(),
                },
                authority: sigil_kernel::DelegationAuthorityRecord::DirectTask {
                    task_id: task_id.clone(),
                },
                profile_id: sigil_kernel::AgentProfileId::new("explore")?,
                role: sigil_kernel::AgentRole::SubagentRead,
                isolation: sigil_kernel::TaskIsolationMode::SharedReadOnly,
                permission_upper_bound_fingerprint: format!("sha256:{}", "d".repeat(64)),
                network_upper_bound: sigil_kernel::NetworkPolicy::Deny,
                tool_contract_fingerprint,
                workspace_snapshot_id: None,
                root_run_fingerprint: format!("sha256:{}", "e".repeat(64)),
                root_cancellation_scope_fingerprint: format!("sha256:{}", "f".repeat(64)),
                expires_at_ms: 100,
            }),
            admitted_at_ms: Some(3),
        },
    ))?;
    session.append_control(ControlEntry::AgentApprovalRoute(
        sigil_kernel::AgentApprovalRouteEntry {
            route_id: sigil_kernel::AgentRouteId::new("tui-cancel-approval")?,
            source_thread_id: thread_id.clone(),
            target_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
            call_id: "call-needs-approval".to_owned(),
            tool_name: "write_file".to_owned(),
            binding: Some(sigil_kernel::AgentApprovalRouteBinding {
                batch_id: None,
                attempt_id: attempt_id.clone(),
                permission_signature: format!("sha256:{}", "1".repeat(64)),
                policy_fingerprint: format!("sha256:{}", "2".repeat(64)),
                source_workspace_id: format!("workspace:{}", "3".repeat(64)),
                isolation: sigil_kernel::TaskIsolationMode::SharedReadOnly,
                requested_at_ms: 4,
                expires_at_ms: u64::MAX,
            }),
            status: sigil_kernel::AgentRouteStatus::Requested,
        },
    ))?;
    session.append_control(ControlEntry::AgentThreadStatusChanged(
        sigil_kernel::AgentThreadStatusChangedEntry {
            thread_id: thread_id.clone(),
            status: sigil_kernel::AgentThreadStatus::Blocked,
            reason: Some("waiting for approval".to_owned()),
            updated_at_ms: Some(5),
        },
    ))?;
    assert_eq!(
        session
            .agent_thread_state_projection()
            .threads
            .get(&thread_id)
            .expect("seeded background child")
            .status,
        sigil_kernel::AgentThreadStatus::Blocked
    );

    let (active, _cancellation, release) = fixture.active(false, false);
    sigil_runtime::agent_supervisor::task_execution::bind_task_run_cancellation_scope(
        fixture
            .state
            .session
            .current
            .as_mut()
            .expect("fixture session remains attached"),
        &task_id,
        &active.cancellation_owner.handle(),
    )?;
    release.send(()).expect("allow run cleanup to finish");
    fixture.settle(active);

    let session = fixture
        .state
        .session
        .current
        .as_ref()
        .expect("reloaded fixture session");
    assert!(matches!(
        session
            .agent_thread_state_projection()
            .approval_routes
            .get(&sigil_kernel::AgentRouteId::new("tui-cancel-approval")?)
            .expect("approval route projection")
            .status,
        sigil_kernel::AgentRouteStatus::Cancelled | sigil_kernel::AgentRouteStatus::Closed
    ));
    assert_eq!(
        session
            .agent_thread_state_projection()
            .threads
            .get(&thread_id)
            .expect("cancelled background child")
            .status,
        sigil_kernel::AgentThreadStatus::Interrupted
    );
    assert_eq!(
        session
            .task_state_projection()
            .tasks
            .get(&task_id)
            .expect("interrupted Direct Task")
            .status,
        sigil_kernel::TaskRunStatus::Interrupted
    );
    Ok(())
}
