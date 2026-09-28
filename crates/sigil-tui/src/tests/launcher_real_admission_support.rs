//! Drives the actual launcher slot owner with a real application/worker supplied by integration fixtures.
use super::*;

pub(crate) fn exercise_slots(
    application: Arc<application_bridge::TuiApplicationSession>,
    sender: runner::WorkerCommandSender,
    config: &sigil_kernel::RootConfig,
    config_path: &Path,
    action: &AppAction,
    count: usize,
    receive: impl FnMut() -> Result<Option<WorkerMessage>>,
) -> Result<Vec<sigil_application::ApplicationCommandRequest>> {
    exercise_actions(
        application,
        sender,
        config,
        config_path,
        (0..count).map(|_| (action.clone(), false)),
        receive,
        |_| Ok(()),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exercise_failed_runs_then_success(
    application: Arc<application_bridge::TuiApplicationSession>,
    sender: runner::WorkerCommandSender,
    config: &sigil_kernel::RootConfig,
    config_path: &Path,
    failing_action: &AppAction,
    failure_count: usize,
    receive: impl FnMut() -> Result<Option<WorkerMessage>>,
    verify_while_owned: impl FnOnce(&[sigil_application::ApplicationCommandRequest]) -> Result<()>,
) -> Result<Vec<sigil_application::ApplicationCommandRequest>> {
    let actions = (0..failure_count)
        .map(|_| (failing_action.clone(), true))
        .chain(std::iter::once((
            AppAction::SubmitPrompt("ordinary input after failed profiles".to_owned()),
            false,
        )));
    exercise_actions(
        application,
        sender,
        config,
        config_path,
        actions,
        receive,
        verify_while_owned,
    )
}

#[allow(clippy::too_many_arguments)]
fn exercise_actions(
    application: Arc<application_bridge::TuiApplicationSession>,
    sender: runner::WorkerCommandSender,
    config: &sigil_kernel::RootConfig,
    config_path: &Path,
    actions: impl IntoIterator<Item = (AppAction, bool)>,
    mut receive: impl FnMut() -> Result<Option<WorkerMessage>>,
    verify_while_owned: impl FnOnce(&[sigil_application::ApplicationCommandRequest]) -> Result<()>,
) -> Result<Vec<sigil_application::ApplicationCommandRequest>> {
    let mut app = AppState::from_root_config(config_path, config);
    let mut worker = Some(WorkerRuntime {
        worker_tx: sender,
        application: Some(application),
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: std::sync::mpsc::channel().1,
        join_handle: None,
        ready: true,
    });
    let mut requests = Vec::new();
    let mut failures = 0;
    for (iteration, (action, expect_failure)) in actions.into_iter().enumerate() {
        app.runtime.run_submission_intent = Arc::new(());
        assert!(
            queue_run_admission(&mut app, &mut worker, &action).with_context(|| format!(
                "iteration={iteration} queue actual application admission"
            ))?
        );
        let pending = &worker.as_ref().context("worker")?.pending_interactions;
        assert_eq!(
            pending.len(),
            1,
            "a new explicit command owns exactly one slot"
        );
        let request = Arc::clone(&pending[0].request);
        let iteration_started = Instant::now();
        let deadline = iteration_started + Duration::from_secs(10);
        let mut terminal = false;
        let mut failure = None;
        let mut task_status = None;
        let mut observed_messages = 0;
        loop {
            while let Some(message) = receive()? {
                apply_worker_message_state(worker.as_mut().context("worker")?, None, &message);
                observed_messages += 1;
                let phase = match &message {
                    WorkerMessage::LivePreviewSource { .. } => "preview_source",
                    WorkerMessage::LivePreviewDurableFrontier { .. } => "durable_frontier",
                    WorkerMessage::RunStarted { .. } => "run_started",
                    WorkerMessage::SkillRunStarted { .. } => "skill_started",
                    WorkerMessage::AgentRunStarted { .. } => "agent_started",
                    WorkerMessage::TaskRunStarted { .. } => "task_started",
                    WorkerMessage::RunFinished { .. } => "run_finished",
                    WorkerMessage::TaskRunFinished { .. } => "task_finished",
                    WorkerMessage::AgentRunFinished { .. } => "agent_finished",
                    WorkerMessage::RunFailed(_) => "run_failed",
                    WorkerMessage::ApplicationRunOwnerReturned { .. } => "owner_returned",
                    WorkerMessage::Event(_) => "run_event",
                    WorkerMessage::Notice(_) => "notice",
                    _ => "other",
                };
                eprintln!(
                    "iteration={iteration} message={phase} kind={:?} elapsed_ms={}",
                    std::mem::discriminant(&message),
                    iteration_started.elapsed().as_millis()
                );
                if let WorkerMessage::RunFailed(error) = &message {
                    failure = Some(sigil_kernel::safe_persistence_text(error));
                    terminal = true;
                }
                if let WorkerMessage::TaskRunFinished {
                    status, entries, ..
                } = &message
                {
                    task_status = Some(*status);
                    if *status != sigil_kernel::TaskRunStatus::Completed {
                        eprintln!(
                            "iteration={iteration} task_status={status:?} controls={:?}",
                            entries
                                .iter()
                                .filter_map(|entry| match entry {
                                    sigil_kernel::SessionLogEntry::Control(
                                        sigil_kernel::ControlEntry::TaskRun(run),
                                    ) => Some((
                                        run.status,
                                        run.reason
                                            .as_deref()
                                            .map(sigil_kernel::safe_persistence_text)
                                    )),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                        );
                    }
                }
                terminal |= matches!(
                    message,
                    WorkerMessage::RunFinished { .. }
                        | WorkerMessage::PlanRunFinished { .. }
                        | WorkerMessage::AgentRunFinished { .. }
                        | WorkerMessage::TaskRunFinished { .. }
                );
            }
            poll_application_admission(&mut app, &mut worker).with_context(|| {
                format!("iteration={iteration} poll actual application admission")
            })?;
            if worker
                .as_ref()
                .context("worker")?
                .pending_interactions
                .is_empty()
                && terminal
            {
                break;
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "real command stalled: iteration={iteration}, terminal={terminal}, messages={observed_messages}, failure={failure:?}, pending={:?}, retained={}",
                worker.as_ref().context("worker")?.pending_interactions,
                app.retained_application_admissions.len()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        if expect_failure {
            assert!(
                failure.is_some(),
                "iteration={iteration} must fail before its domain admission"
            );
            failures += 1;
        } else {
            assert!(
                failure.is_none(),
                "iteration={iteration}: actual run failed: {failure:?}"
            );
            if let Some(status) = task_status {
                assert_eq!(
                    status,
                    sigil_kernel::TaskRunStatus::Completed,
                    "iteration={iteration}"
                );
            }
        }
        assert_eq!(
            app.retained_application_admissions.len(),
            failures,
            "only joined unresolved owners move to recovery"
        );
        for retained in &app.retained_application_admissions {
            assert!(!retained.receipt_resolved && retained.run_owner_returned);
            assert!(retained.handle.is_none() && retained.receiver.is_none());
            assert!(!retained.retryable && !retained.reconcile_requested);
        }
        eprintln!(
            "launcher admission iteration={iteration} terminal={terminal} failure={} active=0 retained={failures}",
            failure.is_some()
        );
        requests.push(
            request
                .lock()
                .map_err(|_| anyhow::anyhow!("request lock"))?
                .clone()
                .context("actual prepared request")?,
        );
        futures::executor::block_on(
            worker
                .as_ref()
                .context("worker")?
                .application
                .as_ref()
                .context("application")?
                .refresh(),
        )
        .with_context(|| {
            format!("iteration={iteration} refresh completed application projection")
        })?;
    }
    // Recovery queries still require the original worker when no durable domain proof exists.
    // Run assertions before this real WorkerRuntime owner drops and requests worker shutdown.
    verify_while_owned(&requests)?;
    Ok(requests)
}
