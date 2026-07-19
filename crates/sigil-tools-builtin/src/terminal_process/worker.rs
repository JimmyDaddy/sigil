use super::*;

#[cfg(unix)]
use crate::process_group::{process_group_has_live_members, send_process_group_signal};
#[cfg(windows)]
use crate::process_owner::terminate_owned_process_tree;

pub(super) struct PtyRuntime {
    pub(super) process_owner: ProcessTreeOwnerGuard,
    pub(super) io_control: Arc<PtyIoControl>,
    pub(super) killer: Arc<StdMutex<Box<dyn ChildKiller + Send + Sync>>>,
    pub(super) process_id: Option<u32>,
    pub(super) capture_ledger: Arc<TerminalCaptureLedger>,
    pub(super) cancel_requested: Arc<AtomicBool>,
    pub(super) wait_task: JoinHandle<PtyWaitOutcome>,
    pub(super) capture_failure_rx: mpsc::UnboundedReceiver<TerminalCaptureFailure>,
    pub(super) child_exit_rx: mpsc::UnboundedReceiver<TerminalTaskStatus>,
}

pub(super) struct PtyWaitOutcome {
    pub(super) status: TerminalTaskStatus,
    pub(super) capture_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
struct TerminalCaptureEvidence {
    observed_total_bytes: u64,
    omitted_observed_bytes: u64,
    limit_bytes: Option<u64>,
    termination_reason: Option<TerminalOutputTerminationReason>,
}

impl TerminalCaptureEvidence {
    fn from_ledger(
        ledger: &TerminalCaptureLedger,
        fallback: Option<TerminalOutputTerminationReason>,
    ) -> Self {
        Self {
            observed_total_bytes: ledger.total_observed_bytes(),
            omitted_observed_bytes: ledger.omitted_observed_bytes(),
            limit_bytes: ledger.limit_bytes(),
            termination_reason: ledger.termination_reason().or(fallback),
        }
    }
}

pub(super) struct TerminalWorker {
    pub(super) _process_owner: ProcessTreeOwnerGuard,
    pub(super) child: TokioChild,
    pub(super) process_id: Option<u32>,
    pub(super) summary: Arc<Mutex<TerminalTaskEntry>>,
    pub(super) artifacts: TerminalTaskArtifacts,
    pub(super) stdout_task: JoinHandle<Result<io::CaptureOutcome>>,
    pub(super) stderr_task: JoinHandle<Result<io::CaptureOutcome>>,
    pub(super) capture_ledger: Arc<TerminalCaptureLedger>,
    pub(super) capture_failure_rx: mpsc::UnboundedReceiver<TerminalCaptureFailure>,
    pub(super) cancel_rx: mpsc::Receiver<CancelCommand>,
    pub(super) preview_limit_bytes: usize,
    pub(super) cancel_grace: Duration,
}

pub(super) struct PtyWorker {
    pub(super) _process_owner: ProcessTreeOwnerGuard,
    pub(super) summary: Arc<Mutex<TerminalTaskEntry>>,
    pub(super) artifacts: TerminalTaskArtifacts,
    pub(super) wait_task: JoinHandle<PtyWaitOutcome>,
    pub(super) killer: Arc<StdMutex<Box<dyn ChildKiller + Send + Sync>>>,
    pub(super) io_control: Arc<PtyIoControl>,
    pub(super) process_id: Option<u32>,
    pub(super) capture_ledger: Arc<TerminalCaptureLedger>,
    pub(super) cancel_requested: Arc<AtomicBool>,
    pub(super) capture_failure_rx: mpsc::UnboundedReceiver<TerminalCaptureFailure>,
    pub(super) child_exit_rx: mpsc::UnboundedReceiver<TerminalTaskStatus>,
    pub(super) preview_limit_bytes: usize,
    pub(super) cancel_grace: Duration,
}

struct CaptureTaskState {
    stream: TerminalOutputStream,
    task: Option<JoinHandle<Result<io::CaptureOutcome>>>,
    failure: Option<String>,
}

impl CaptureTaskState {
    fn new(stream: TerminalOutputStream, task: JoinHandle<Result<io::CaptureOutcome>>) -> Self {
        Self {
            stream,
            task: Some(task),
            failure: None,
        }
    }

    fn is_pending(&self) -> bool {
        self.task.is_some()
    }

    async fn wait_for_completion(
        &mut self,
    ) -> std::result::Result<Result<io::CaptureOutcome>, tokio::task::JoinError> {
        match self.task.as_mut() {
            Some(task) => task.await,
            None => std::future::pending().await,
        }
    }

    fn record_completion(
        &mut self,
        result: std::result::Result<Result<io::CaptureOutcome>, tokio::task::JoinError>,
    ) -> Option<String> {
        self.task = None;
        let stream = match self.stream {
            TerminalOutputStream::Stdout => "stdout",
            TerminalOutputStream::Stderr => "stderr",
        };
        let failure = match result {
            Ok(Ok(_)) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(error) => Some(format!("terminal {stream} capture task failed: {error}")),
        };
        self.failure = failure.clone();
        failure
    }

    async fn join(&mut self) -> Option<String> {
        if !self.is_pending() {
            return self.failure.clone();
        }
        let result = self.wait_for_completion().await;
        self.record_completion(result)
    }

    async fn abort_and_join(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

pub(super) fn spawn_pty_runtime(
    plan: &TerminalTaskStartPlan,
    size: TerminalPtySize,
    artifact_limits: TerminalArtifactLimits,
) -> Result<PtyRuntime> {
    let pty_system = native_pty_system();
    let portable_pty::PtyPair { master, slave } = pty_system
        .openpty(size.to_portable())
        .context("failed to open terminal pty")?;
    let reader = master
        .try_clone_reader()
        .context("failed to clone terminal pty reader")?;
    let writer = master
        .take_writer()
        .context("failed to take terminal pty writer")?;
    let (input_tx, input_rx) = std_mpsc::sync_channel(TERMINAL_PTY_INPUT_QUEUE_BOUND);
    spawn_pty_input_thread(writer, input_rx);
    let io_control = Arc::new(PtyIoControl::new(input_tx, master));
    #[cfg(windows)]
    let terminal_io_control = Some(Arc::downgrade(&io_control));
    #[cfg(not(windows))]
    let terminal_io_control = None;

    let command_spec = plan
        .pty_command
        .as_ref()
        .ok_or_else(|| anyhow!("terminal pty command plan is unavailable"))?;
    let mut command = CommandBuilder::new(&command_spec.program);
    for arg in &command_spec.args {
        command.arg(arg);
    }
    command.cwd(&command_spec.cwd);
    for (key, value) in &command_spec.env {
        command.env(key, value);
    }
    let mut child = slave
        .spawn_command(command)
        .with_context(|| format!("failed to start terminal pty command: {}", plan.command))?;
    let process_id = child.process_id();
    let process_owner = match ProcessTreeOwnerGuard::assign(process_id) {
        Ok(owner) => owner,
        Err(error) => {
            let direct_kill = child.kill();
            let direct_wait = child.wait();
            bail!(
                "failed to establish terminal PTY process-tree ownership: {error}; direct_kill={direct_kill:?}, direct_wait={direct_wait:?}"
            );
        }
    };
    let killer = Arc::new(StdMutex::new(child.clone_killer()));
    let cancel_requested = Arc::new(AtomicBool::new(false));
    let (capture_failure_tx, capture_failure_rx) = mpsc::unbounded_channel();
    let capture_ledger = Arc::new(TerminalCaptureLedger::default());
    let (child_exit_tx, child_exit_rx) = mpsc::unbounded_channel();
    let read_thread = spawn_pty_read_thread(
        reader,
        plan.artifacts.absolute_stdout.clone(),
        plan.artifacts.absolute_output.clone(),
        artifact_limits,
        terminal_io_control,
        Arc::clone(&capture_ledger),
        capture_failure_tx,
    );
    let wait_task = task::spawn_blocking(move || {
        let wait_result = child.wait();
        let status = status_from_pty_wait_result(wait_result);
        let _ = child_exit_tx.send(status.clone());
        let capture_error = join_pty_read_thread(read_thread);
        PtyWaitOutcome {
            status,
            capture_error,
        }
    });

    Ok(PtyRuntime {
        process_owner,
        io_control,
        killer,
        process_id,
        capture_ledger,
        cancel_requested,
        wait_task,
        capture_failure_rx,
        child_exit_rx,
    })
}

pub(super) async fn run_terminal_worker(mut worker: TerminalWorker) {
    enum WorkerEvent {
        ProcessExited(TerminalTaskStatus),
        Cancel(CancelCommand),
        CaptureFailed(String),
    }

    let mut stdout_task = CaptureTaskState::new(TerminalOutputStream::Stdout, worker.stdout_task);
    let mut stderr_task = CaptureTaskState::new(TerminalOutputStream::Stderr, worker.stderr_task);
    let mut capture_open = true;
    let mut cancel_open = true;
    let event = loop {
        tokio::select! {
            biased;
            failure = worker.capture_failure_rx.recv(), if capture_open => {
                if let Some(failure) = failure {
                    break WorkerEvent::CaptureFailed(failure.to_string());
                }
                capture_open = false;
            }
            cancel = worker.cancel_rx.recv(), if cancel_open => {
                if let Some(cancel) = cancel {
                    break WorkerEvent::Cancel(cancel);
                }
                cancel_open = false;
            }
            result = stdout_task.wait_for_completion(), if stdout_task.is_pending() => {
                if let Some(failure) = stdout_task.record_completion(result) {
                    break WorkerEvent::CaptureFailed(failure);
                }
            }
            result = stderr_task.wait_for_completion(), if stderr_task.is_pending() => {
                if let Some(failure) = stderr_task.record_completion(result) {
                    break WorkerEvent::CaptureFailed(failure);
                }
            }
            wait_result = worker.child.wait() => {
                break WorkerEvent::ProcessExited(status_from_wait_result(wait_result));
            }
        }
    };

    match event {
        WorkerEvent::ProcessExited(status) => {
            let _ = finalize_terminal_task_after_process_exit(
                &worker.summary,
                &worker.artifacts,
                status,
                stdout_task,
                stderr_task,
                Arc::clone(&worker.capture_ledger),
                worker.preview_limit_bytes,
                worker.process_id,
                worker.cancel_grace,
            )
            .await;
        }
        WorkerEvent::Cancel(cancel) => {
            let (status, cleanup) = cancel_child_with_cleanup(
                &mut worker.child,
                worker.process_id,
                worker.cancel_grace,
            )
            .await;
            let entry = finalize_terminal_task_with_override(
                &worker.summary,
                &worker.artifacts,
                status,
                stdout_task,
                stderr_task,
                Arc::clone(&worker.capture_ledger),
                worker.preview_limit_bytes,
                Some(cleanup),
                None,
            )
            .await;
            let _ = cancel.respond_to.send(entry);
        }
        WorkerEvent::CaptureFailed(failure) => {
            let cleanup = terminate_process_tree_after_capture_failure(
                &mut worker.child,
                worker.process_id,
                worker.cancel_grace,
            )
            .await;
            let status = TerminalTaskStatus::Failed { reason: failure };
            let _ = finalize_terminal_task_with_override(
                &worker.summary,
                &worker.artifacts,
                status,
                stdout_task,
                stderr_task,
                Arc::clone(&worker.capture_ledger),
                worker.preview_limit_bytes,
                Some(cleanup),
                Some(TerminalOutputTerminationReason::OutputCaptureFailed),
            )
            .await;
        }
    }
}

async fn finalize_terminal_task_after_process_exit(
    summary: &Arc<Mutex<TerminalTaskEntry>>,
    artifacts: &TerminalTaskArtifacts,
    status: TerminalTaskStatus,
    mut stdout_task: CaptureTaskState,
    mut stderr_task: CaptureTaskState,
    capture_ledger: Arc<TerminalCaptureLedger>,
    preview_limit_bytes: usize,
    process_id: Option<u32>,
    drain_grace: Duration,
) -> TerminalTaskEntry {
    match timeout(
        drain_grace.max(Duration::from_millis(50)),
        join_capture_tasks(&mut stdout_task, &mut stderr_task),
    )
    .await
    {
        Ok(None) => {
            finalize_terminal_summary(
                summary,
                artifacts,
                status,
                None,
                preview_limit_bytes,
                None,
                TerminalCaptureEvidence::from_ledger(&capture_ledger, None),
            )
            .await
        }
        Ok(Some(error)) => {
            let cleanup = cleanup_process_group_after_direct_exit(process_id).await;
            finalize_terminal_summary(
                summary,
                artifacts,
                TerminalTaskStatus::Failed {
                    reason: format!("failed to capture terminal output: {error}"),
                },
                Some(error),
                preview_limit_bytes,
                Some(cleanup),
                TerminalCaptureEvidence::from_ledger(
                    &capture_ledger,
                    Some(TerminalOutputTerminationReason::OutputCaptureFailed),
                ),
            )
            .await
        }
        Err(_) => {
            let cleanup = cleanup_process_group_after_direct_exit(process_id).await;
            let second_wait = timeout(
                drain_grace.max(Duration::from_millis(50)),
                join_capture_tasks(&mut stdout_task, &mut stderr_task),
            )
            .await;
            if second_wait.is_err() {
                tokio::join!(stdout_task.abort_and_join(), stderr_task.abort_and_join());
            }
            finalize_terminal_summary(
                summary,
                artifacts,
                TerminalTaskStatus::Failed {
                    reason: "terminal output reader drain timed out after direct child exit"
                        .to_owned(),
                },
                Some("terminal output reader drain timed out".to_owned()),
                preview_limit_bytes,
                Some(cleanup),
                TerminalCaptureEvidence::from_ledger(
                    &capture_ledger,
                    Some(TerminalOutputTerminationReason::OutputDrainTimeout),
                ),
            )
            .await
        }
    }
}

async fn join_capture_tasks(
    stdout_task: &mut CaptureTaskState,
    stderr_task: &mut CaptureTaskState,
) -> Option<String> {
    let (stdout, stderr) = tokio::join!(stdout_task.join(), stderr_task.join());
    match (stdout, stderr) {
        (Some(error), _) | (_, Some(error)) => Some(error),
        (None, None) => None,
    }
}

pub(super) async fn run_pty_worker(mut worker: PtyWorker) {
    enum PtyWorkerEvent {
        CaptureFailed(TerminalCaptureFailure),
        ChildExited(TerminalTaskStatus),
        WaitCompleted(std::result::Result<PtyWaitOutcome, tokio::task::JoinError>),
    }
    let mut capture_open = true;
    let mut child_exit_open = true;
    let event = loop {
        let event = tokio::select! {
        biased;
            failure = worker.capture_failure_rx.recv(), if capture_open => {
                if failure.is_none() {
                    capture_open = false;
                }
                failure.map(PtyWorkerEvent::CaptureFailed)
            }
            child_exit = worker.child_exit_rx.recv(), if child_exit_open => {
                if child_exit.is_none() {
                    child_exit_open = false;
                }
                child_exit.map(PtyWorkerEvent::ChildExited)
            }
            outcome = &mut worker.wait_task => {
                Some(PtyWorkerEvent::WaitCompleted(outcome))
            }
        };
        if let Some(event) = event {
            break event;
        }
    };

    match event {
        PtyWorkerEvent::WaitCompleted(outcome) => {
            worker.io_control.close();
            let mut outcome = joined_pty_outcome(outcome);
            let cleanup = if worker.cancel_requested.load(Ordering::SeqCst) {
                let cleanup = cleanup_process_group_after_direct_exit(worker.process_id).await;
                outcome.status = if cleanup.status == ExecutionCleanupStatus::Completed {
                    TerminalTaskStatus::Cancelled
                } else {
                    TerminalTaskStatus::Interrupted
                };
                Some(cleanup)
            } else {
                None
            };
            let fallback = outcome
                .capture_error
                .as_ref()
                .map(|_| TerminalOutputTerminationReason::OutputCaptureFailed);
            let _ = finalize_terminal_summary(
                &worker.summary,
                &worker.artifacts,
                outcome.status,
                outcome.capture_error,
                worker.preview_limit_bytes,
                cleanup,
                TerminalCaptureEvidence::from_ledger(&worker.capture_ledger, fallback),
            )
            .await;
        }
        PtyWorkerEvent::CaptureFailed(failure) => {
            worker.io_control.close();
            let (outcome, cleanup) = terminate_pty_after_capture_failure(&mut worker).await;
            let _ = finalize_terminal_summary(
                &worker.summary,
                &worker.artifacts,
                TerminalTaskStatus::Failed {
                    reason: failure.to_string(),
                },
                outcome.capture_error,
                worker.preview_limit_bytes,
                Some(cleanup),
                TerminalCaptureEvidence::from_ledger(
                    &worker.capture_ledger,
                    Some(TerminalOutputTerminationReason::OutputCaptureFailed),
                ),
            )
            .await;
        }
        PtyWorkerEvent::ChildExited(status) => {
            worker.io_control.close();
            finalize_pty_after_child_exit(&mut worker, status).await;
        }
    }
}

async fn finalize_pty_after_child_exit(worker: &mut PtyWorker, status: TerminalTaskStatus) {
    if let Ok(outcome) = timeout(
        worker.cancel_grace.max(Duration::from_millis(50)),
        &mut worker.wait_task,
    )
    .await
    {
        let mut outcome = joined_pty_outcome(outcome);
        let cleanup = if worker.cancel_requested.load(Ordering::SeqCst) {
            let cleanup = cleanup_process_group_after_direct_exit(worker.process_id).await;
            outcome.status = if cleanup.status == ExecutionCleanupStatus::Completed {
                TerminalTaskStatus::Cancelled
            } else {
                TerminalTaskStatus::Interrupted
            };
            Some(cleanup)
        } else {
            None
        };
        let fallback = outcome
            .capture_error
            .as_ref()
            .map(|_| TerminalOutputTerminationReason::OutputCaptureFailed);
        let _ = finalize_terminal_summary(
            &worker.summary,
            &worker.artifacts,
            outcome.status,
            outcome.capture_error,
            worker.preview_limit_bytes,
            cleanup,
            TerminalCaptureEvidence::from_ledger(&worker.capture_ledger, fallback),
        )
        .await;
        return;
    }

    let cleanup = cleanup_process_group_after_direct_exit(worker.process_id).await;
    let drained = timeout(
        worker.cancel_grace.max(Duration::from_millis(50)),
        &mut worker.wait_task,
    )
    .await;
    let drain_converged = drained.is_ok();
    let capture_error = match drained {
        Ok(outcome) => joined_pty_outcome(outcome).capture_error,
        Err(_) => {
            worker.wait_task.abort();
            Some(
                "terminal pty output reader did not converge after process-tree cleanup".to_owned(),
            )
        }
    };
    let final_status = pty_status_after_cleanup_drain(
        &status,
        worker.cancel_requested.load(Ordering::SeqCst),
        &cleanup,
        drain_converged,
    );
    let _ = finalize_terminal_summary(
        &worker.summary,
        &worker.artifacts,
        final_status,
        capture_error,
        worker.preview_limit_bytes,
        Some(cleanup),
        TerminalCaptureEvidence::from_ledger(
            &worker.capture_ledger,
            Some(TerminalOutputTerminationReason::OutputDrainTimeout),
        ),
    )
    .await;
}

pub(super) fn pty_status_after_cleanup_drain(
    observed_status: &TerminalTaskStatus,
    cancel_requested: bool,
    cleanup: &ExecutionCleanupReceipt,
    drain_converged: bool,
) -> TerminalTaskStatus {
    if drain_converged && cancel_requested {
        return if cleanup.status == ExecutionCleanupStatus::Completed {
            TerminalTaskStatus::Cancelled
        } else {
            TerminalTaskStatus::Interrupted
        };
    }

    TerminalTaskStatus::Failed {
        reason: format!(
            "terminal pty output reader drain timed out after child status {}",
            observed_status.as_str()
        ),
    }
}

fn joined_pty_outcome(
    outcome: std::result::Result<PtyWaitOutcome, tokio::task::JoinError>,
) -> PtyWaitOutcome {
    match outcome {
        Ok(outcome) => outcome,
        Err(error) => PtyWaitOutcome {
            status: TerminalTaskStatus::Failed {
                reason: format!("terminal pty wait task failed: {error}"),
            },
            capture_error: None,
        },
    }
}

#[cfg(test)]
pub(super) async fn finalize_terminal_task(
    summary: &Arc<Mutex<TerminalTaskEntry>>,
    artifacts: &TerminalTaskArtifacts,
    status: TerminalTaskStatus,
    stdout_task: JoinHandle<Result<io::CaptureOutcome>>,
    stderr_task: JoinHandle<Result<io::CaptureOutcome>>,
    capture_ledger: Arc<TerminalCaptureLedger>,
    preview_limit_bytes: usize,
) -> TerminalTaskEntry {
    finalize_terminal_task_states(
        summary,
        artifacts,
        status,
        CaptureTaskState::new(TerminalOutputStream::Stdout, stdout_task),
        CaptureTaskState::new(TerminalOutputStream::Stderr, stderr_task),
        capture_ledger,
        preview_limit_bytes,
    )
    .await
}

#[cfg(test)]
async fn finalize_terminal_task_states(
    summary: &Arc<Mutex<TerminalTaskEntry>>,
    artifacts: &TerminalTaskArtifacts,
    status: TerminalTaskStatus,
    stdout_task: CaptureTaskState,
    stderr_task: CaptureTaskState,
    capture_ledger: Arc<TerminalCaptureLedger>,
    preview_limit_bytes: usize,
) -> TerminalTaskEntry {
    finalize_terminal_task_with_override(
        summary,
        artifacts,
        status,
        stdout_task,
        stderr_task,
        capture_ledger,
        preview_limit_bytes,
        None,
        None,
    )
    .await
}

async fn finalize_terminal_task_with_override(
    summary: &Arc<Mutex<TerminalTaskEntry>>,
    artifacts: &TerminalTaskArtifacts,
    status: TerminalTaskStatus,
    mut stdout_task: CaptureTaskState,
    mut stderr_task: CaptureTaskState,
    capture_ledger: Arc<TerminalCaptureLedger>,
    preview_limit_bytes: usize,
    cleanup_override: Option<ExecutionCleanupReceipt>,
    fallback_termination: Option<TerminalOutputTerminationReason>,
) -> TerminalTaskEntry {
    let (capture_error, join_fallback) = match timeout(
        Duration::from_millis(DEFAULT_CANCEL_GRACE_MS),
        join_capture_tasks(&mut stdout_task, &mut stderr_task),
    )
    .await
    {
        Ok(error) => {
            let fallback = error
                .as_ref()
                .map(|_| TerminalOutputTerminationReason::OutputCaptureFailed);
            (error, fallback)
        }
        Err(_) => {
            tokio::join!(stdout_task.abort_and_join(), stderr_task.abort_and_join());
            (
                Some("terminal output reader drain timed out during finalization".to_owned()),
                Some(TerminalOutputTerminationReason::OutputDrainTimeout),
            )
        }
    };
    let fallback_termination = fallback_termination.or(join_fallback);

    finalize_terminal_summary(
        summary,
        artifacts,
        status,
        capture_error,
        preview_limit_bytes,
        cleanup_override,
        TerminalCaptureEvidence::from_ledger(&capture_ledger, fallback_termination),
    )
    .await
}

async fn finalize_terminal_summary(
    summary: &Arc<Mutex<TerminalTaskEntry>>,
    artifacts: &TerminalTaskArtifacts,
    status: TerminalTaskStatus,
    capture_error: Option<String>,
    preview_limit_bytes: usize,
    cleanup_override: Option<ExecutionCleanupReceipt>,
    mut capture_evidence: TerminalCaptureEvidence,
) -> TerminalTaskEntry {
    let mut final_status = status;
    if let Some(error) = capture_error
        && matches!(final_status, TerminalTaskStatus::Exited { .. })
    {
        final_status = TerminalTaskStatus::Failed {
            reason: format!("failed to capture terminal output: {error}"),
        };
    }

    let (log_summary, summary_error) =
        match summarize_log(&artifacts.absolute_output, preview_limit_bytes).await {
            Ok(summary) => (summary, None),
            Err(error) => (
                LogSummary {
                    preview: format!("failed to summarize terminal output: {error}"),
                    sha256: String::new(),
                    truncated: false,
                    total_bytes: 0,
                },
                Some(error.to_string()),
            ),
        };
    if summary_error.is_some() && capture_evidence.termination_reason.is_none() {
        capture_evidence.termination_reason =
            Some(TerminalOutputTerminationReason::OutputCaptureFailed);
    }
    if let Some(error) = summary_error
        && matches!(final_status, TerminalTaskStatus::Exited { .. })
    {
        final_status = TerminalTaskStatus::Failed {
            reason: format!("failed to summarize terminal output: {error}"),
        };
    }

    if matches!(final_status, TerminalTaskStatus::Cancelled)
        && !cleanup_override
            .as_ref()
            .is_some_and(|receipt| receipt.status == ExecutionCleanupStatus::Completed)
    {
        final_status = TerminalTaskStatus::Interrupted;
    }
    let mut entry = summary.lock().await;
    entry.status = final_status;
    entry.output_preview = (!log_summary.preview.is_empty()).then_some(log_summary.preview);
    entry.output_hash = (!log_summary.sha256.is_empty()).then_some(log_summary.sha256);
    entry.output_truncated = log_summary.truncated
        || capture_evidence.omitted_observed_bytes > 0
        || capture_evidence.termination_reason.is_some();
    entry.output_total_bytes = capture_evidence
        .observed_total_bytes
        .max(log_summary.total_bytes);
    entry.output_limit_bytes = capture_evidence.limit_bytes;
    entry.output_termination_reason = capture_evidence.termination_reason;
    entry.cleanup = if matches!(entry.status, TerminalTaskStatus::Cancelled) {
        cleanup_override
    } else {
        cleanup_override.or_else(|| terminal_cleanup_receipt_for_status(&entry.status))
    };
    entry.updated_at_ms = current_epoch_ms();
    let cloned = entry.clone();
    drop(entry);
    let _ = write_task_meta(&artifacts.absolute_meta, &cloned).await;
    cloned
}

pub(super) fn status_from_pty_wait_result(
    wait_result: std::io::Result<portable_pty::ExitStatus>,
) -> TerminalTaskStatus {
    match wait_result {
        Ok(status) => TerminalTaskStatus::Exited {
            exit_code: status
                .signal()
                .is_none()
                .then(|| i32::try_from(status.exit_code()).unwrap_or(i32::MAX)),
        },
        Err(error) => TerminalTaskStatus::Failed {
            reason: format!("terminal pty wait failed: {error}"),
        },
    }
}

pub(super) fn status_from_wait_result(
    wait_result: std::io::Result<std::process::ExitStatus>,
) -> TerminalTaskStatus {
    match wait_result {
        Ok(status) => TerminalTaskStatus::Exited {
            exit_code: status.code(),
        },
        Err(error) => TerminalTaskStatus::Failed {
            reason: format!("terminal process wait failed: {error}"),
        },
    }
}

#[cfg(unix)]
async fn terminate_process_tree_after_capture_failure(
    child: &mut TokioChild,
    process_id: Option<u32>,
    grace: Duration,
) -> ExecutionCleanupReceipt {
    let Some(process_id) = process_id else {
        let direct_kill = child.start_kill();
        let direct_wait = timeout(TERMINAL_CLEANUP_WAIT_TIMEOUT, child.wait()).await;
        return ExecutionCleanupReceipt::unknown(format!(
            "terminal capture failed; process-tree identity was unavailable: direct_kill={direct_kill:?}, direct_wait={direct_wait:?}"
        ));
    };

    let term_result = send_process_group_signal(process_id, "TERM").await;
    let initial_wait = timeout(grace, child.wait()).await;
    let direct_reaped = matches!(initial_wait, Ok(Ok(_)));
    let wait_error = match initial_wait {
        Ok(Err(error)) => Some(error.to_string()),
        _ => None,
    };
    let kill_result = send_process_group_signal(process_id, "KILL").await;
    let final_wait = if direct_reaped {
        Ok(())
    } else {
        match timeout(grace.max(Duration::from_millis(50)), child.wait()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => {
                let direct_kill = child.start_kill();
                let direct_wait = timeout(TERMINAL_CLEANUP_WAIT_TIMEOUT, child.wait()).await;
                Err(format!(
                    "process-group kill wait failed: {error}; direct_kill={direct_kill:?}, direct_wait={direct_wait:?}"
                ))
            }
            Err(_) => {
                let direct_kill = child.start_kill();
                match timeout(TERMINAL_CLEANUP_WAIT_TIMEOUT, child.wait()).await {
                    Ok(Ok(_)) => Ok(()),
                    Ok(Err(error)) => Err(format!(
                        "direct kill fallback failed to reap child: kill={direct_kill:?}, wait={error}"
                    )),
                    Err(_) => Err(format!(
                        "direct kill fallback did not reap child within {:?}: kill={direct_kill:?}",
                        TERMINAL_CLEANUP_WAIT_TIMEOUT
                    )),
                }
            }
        }
    };
    let group_exit = wait_for_terminal_process_group_exit(process_id).await;

    if final_wait.is_ok() && matches!(group_exit, Ok(true)) {
        return ExecutionCleanupReceipt::completed(format!(
            "terminal output capture failed; process group {process_id} was terminated and direct child reaped"
        ));
    }
    ExecutionCleanupReceipt::failed(format!(
        "terminal output capture failed; process-tree cleanup was not fully proven: term={}, kill={}, wait={}, group_exit={group_exit:?}{}",
        result_reason(&term_result),
        result_reason(&kill_result),
        result_reason(&final_wait),
        wait_error
            .map(|error| format!(", initial_wait={error}"))
            .unwrap_or_default()
    ))
}

#[cfg(windows)]
async fn terminate_process_tree_after_capture_failure(
    child: &mut TokioChild,
    process_id: Option<u32>,
    grace: Duration,
) -> ExecutionCleanupReceipt {
    let terminate = process_id
        .ok_or_else(|| anyhow!("terminal process id unavailable"))
        .and_then(terminate_owned_process_tree);
    let wait = timeout(grace.max(Duration::from_millis(50)), child.wait()).await;
    if terminate.is_ok() && matches!(wait, Ok(Ok(_))) {
        return ExecutionCleanupReceipt::completed(
            "terminal output capture failed; Windows Job Object terminated the process tree and reaped the direct child",
        );
    }
    let direct_kill = child.start_kill();
    let direct_wait = timeout(TERMINAL_CLEANUP_WAIT_TIMEOUT, child.wait()).await;
    ExecutionCleanupReceipt::failed(format!(
        "terminal output capture failed; Windows Job Object cleanup was not proven: terminate={terminate:?}, wait={wait:?}, direct_kill={direct_kill:?}, direct_wait={direct_wait:?}"
    ))
}

#[cfg(not(any(unix, windows)))]
async fn terminate_process_tree_after_capture_failure(
    child: &mut TokioChild,
    _process_id: Option<u32>,
    _grace: Duration,
) -> ExecutionCleanupReceipt {
    let kill = child.start_kill();
    let wait = timeout(TERMINAL_CLEANUP_WAIT_TIMEOUT, child.wait()).await;
    ExecutionCleanupReceipt::unsupported(format!(
        "terminal output capture failed; only the direct child cleanup was attempted on this platform: kill={kill:?}, wait={wait:?}"
    ))
}

#[cfg(unix)]
async fn terminate_pty_after_capture_failure(
    worker: &mut PtyWorker,
) -> (PtyWaitOutcome, ExecutionCleanupReceipt) {
    let term_result = match worker.process_id {
        Some(process_id) => send_process_group_signal(process_id, "TERM").await,
        None => Err(anyhow!("terminal pty process id unavailable")),
    };
    if let Ok(outcome) = timeout(worker.cancel_grace, &mut worker.wait_task).await {
        let outcome = joined_pty_outcome(outcome);
        let kill_result = match worker.process_id {
            Some(process_id) => send_process_group_signal(process_id, "KILL").await,
            None => Err(anyhow!("terminal pty process id unavailable")),
        };
        let group_exit = match worker.process_id {
            Some(process_id) => wait_for_terminal_process_group_exit(process_id).await,
            None => Err(anyhow!("terminal pty process id unavailable")),
        };
        let cleanup = if matches!(group_exit, Ok(true)) {
            ExecutionCleanupReceipt::completed(
                "terminal pty output capture failed; process group was terminated and child reaped",
            )
        } else {
            ExecutionCleanupReceipt::failed(format!(
                "terminal pty output capture failed; process-tree cleanup was not proven: term={}, kill={}, group_exit={group_exit:?}",
                result_reason(&term_result),
                result_reason(&kill_result)
            ))
        };
        return (outcome, cleanup);
    }

    let kill_result = match worker.process_id {
        Some(process_id) => send_process_group_signal(process_id, "KILL").await,
        None => Err(anyhow!("terminal pty process id unavailable")),
    };
    let direct_kill = kill_pty_child(Arc::clone(&worker.killer)).await;
    let waited = timeout(
        worker.cancel_grace.max(Duration::from_millis(50)),
        &mut worker.wait_task,
    )
    .await;
    let wait_converged = waited.is_ok();
    let outcome = match waited {
        Ok(outcome) => joined_pty_outcome(outcome),
        Err(_) => PtyWaitOutcome {
            status: TerminalTaskStatus::Failed {
                reason: "terminal pty wait did not converge after output capture failure"
                    .to_owned(),
            },
            capture_error: None,
        },
    };
    let group_exit = match worker.process_id {
        Some(process_id) => wait_for_terminal_process_group_exit(process_id).await,
        None => Err(anyhow!("terminal pty process id unavailable")),
    };
    let cleanup = if wait_converged && matches!(group_exit, Ok(true)) {
        ExecutionCleanupReceipt::completed(
            "terminal pty output capture failed; process group was killed and child reaped",
        )
    } else {
        ExecutionCleanupReceipt::failed(format!(
            "terminal pty output capture failed; process-tree cleanup was not fully proven: term={}, kill={}, direct_kill={direct_kill:?}, wait_converged={}, group_exit={group_exit:?}",
            result_reason(&term_result),
            result_reason(&kill_result),
            wait_converged
        ))
    };
    (outcome, cleanup)
}

#[cfg(not(unix))]
async fn terminate_pty_after_capture_failure(
    worker: &mut PtyWorker,
) -> (PtyWaitOutcome, ExecutionCleanupReceipt) {
    #[cfg(windows)]
    let terminate = worker
        .process_id
        .ok_or_else(|| anyhow!("terminal pty process id unavailable"))
        .and_then(terminate_owned_process_tree);
    let direct_kill = kill_pty_child(Arc::clone(&worker.killer)).await;
    let waited = timeout(
        worker.cancel_grace.max(Duration::from_millis(50)),
        &mut worker.wait_task,
    )
    .await;
    let wait_converged = waited.is_ok();
    let outcome = match waited {
        Ok(outcome) => joined_pty_outcome(outcome),
        Err(_) => PtyWaitOutcome {
            status: TerminalTaskStatus::Failed {
                reason: "terminal pty wait did not converge after output capture failure"
                    .to_owned(),
            },
            capture_error: None,
        },
    };
    #[cfg(windows)]
    let cleanup = if terminate.is_ok() && wait_converged {
        ExecutionCleanupReceipt::completed(
            "terminal pty output capture failed; Windows Job Object terminated the process tree and child was reaped",
        )
    } else {
        ExecutionCleanupReceipt::failed(format!(
            "terminal pty output capture failed; Windows Job Object cleanup was not proven: terminate={terminate:?}, direct_kill={direct_kill:?}, wait_converged={}",
            wait_converged
        ))
    };
    #[cfg(not(windows))]
    let cleanup = ExecutionCleanupReceipt::unsupported(format!(
        "terminal pty output capture failed; only direct child cleanup was attempted on this platform: direct_kill={direct_kill:?}, wait_converged={}",
        wait_converged
    ));
    (outcome, cleanup)
}

fn result_reason<T, E: std::fmt::Display>(result: &std::result::Result<T, E>) -> String {
    match result {
        Ok(_) => "ok".to_owned(),
        Err(error) => error.to_string(),
    }
}

#[cfg(unix)]
async fn cleanup_process_group_after_direct_exit(
    process_id: Option<u32>,
) -> ExecutionCleanupReceipt {
    let Some(process_id) = process_id else {
        return ExecutionCleanupReceipt::unknown(
            "terminal output capture failed after direct child exit; process-group identity was unavailable",
        );
    };
    let kill_result = send_process_group_signal(process_id, "KILL").await;
    let group_exit = wait_for_terminal_process_group_exit(process_id).await;
    if matches!(group_exit, Ok(true)) {
        ExecutionCleanupReceipt::completed(format!(
            "terminal output capture failed after direct child exit; remaining process group {process_id} was killed"
        ))
    } else {
        ExecutionCleanupReceipt::failed(format!(
            "terminal output capture failed after direct child exit; remaining process-group cleanup was not proven: kill={}, group_exit={group_exit:?}",
            result_reason(&kill_result)
        ))
    }
}

#[cfg(windows)]
async fn cleanup_process_group_after_direct_exit(
    process_id: Option<u32>,
) -> ExecutionCleanupReceipt {
    let Some(process_id) = process_id else {
        return ExecutionCleanupReceipt::unknown(
            "terminal output capture failed after direct child exit; process id was unavailable",
        );
    };
    match terminate_owned_process_tree(process_id) {
        Ok(()) => ExecutionCleanupReceipt::completed(
            "terminal output capture failed after direct child exit; Windows Job Object terminated the remaining process tree",
        ),
        result => ExecutionCleanupReceipt::failed(format!(
            "terminal output capture failed after direct child exit; Windows Job Object cleanup was not proven: {result:?}"
        )),
    }
}

#[cfg(not(any(unix, windows)))]
async fn cleanup_process_group_after_direct_exit(
    _process_id: Option<u32>,
) -> ExecutionCleanupReceipt {
    ExecutionCleanupReceipt::unsupported(
        "terminal output capture failed after direct child exit; process-tree cleanup is unsupported on this platform",
    )
}

#[cfg(test)]
pub(super) async fn cancel_child(
    child: &mut TokioChild,
    process_id: Option<u32>,
    cancel_grace: Duration,
) -> TerminalTaskStatus {
    cancel_child_with_cleanup(child, process_id, cancel_grace)
        .await
        .0
}

async fn cancel_child_with_cleanup(
    child: &mut TokioChild,
    process_id: Option<u32>,
    cancel_grace: Duration,
) -> (TerminalTaskStatus, ExecutionCleanupReceipt) {
    if let Some(process_id) = process_id {
        let _ = send_terminate_signal(process_id).await;
    }

    match timeout(cancel_grace, child.wait()).await {
        Ok(Ok(_)) => cancellation_status_after_process_tree_cleanup(process_id).await,
        Ok(Err(error)) => (
            TerminalTaskStatus::Failed {
                reason: format!("terminal process cancel wait failed: {error}"),
            },
            ExecutionCleanupReceipt::failed("direct child cancel wait failed"),
        ),
        Err(_) => match child.start_kill() {
            Ok(()) => match timeout(TERMINAL_CLEANUP_WAIT_TIMEOUT, child.wait()).await {
                Ok(Ok(_)) => cancellation_status_after_process_tree_cleanup(process_id).await,
                Ok(Err(error)) => (
                    TerminalTaskStatus::Failed {
                        reason: format!("terminal process kill wait failed: {error}"),
                    },
                    ExecutionCleanupReceipt::failed("direct child kill wait failed"),
                ),
                Err(_) => (
                    TerminalTaskStatus::Interrupted,
                    ExecutionCleanupReceipt::failed(format!(
                        "terminal process kill wait exceeded {:?}",
                        TERMINAL_CLEANUP_WAIT_TIMEOUT
                    )),
                ),
            },
            Err(error) => (
                TerminalTaskStatus::Failed {
                    reason: format!("failed to kill terminal process: {error}"),
                },
                ExecutionCleanupReceipt::failed("direct child kill request failed"),
            ),
        },
    }
}

async fn cancellation_status_after_process_tree_cleanup(
    process_id: Option<u32>,
) -> (TerminalTaskStatus, ExecutionCleanupReceipt) {
    let cleanup = cleanup_process_group_after_direct_exit(process_id).await;
    let status = if cleanup.status == ExecutionCleanupStatus::Completed {
        TerminalTaskStatus::Cancelled
    } else {
        TerminalTaskStatus::Interrupted
    };
    (status, cleanup)
}

pub(super) async fn cancel_pty_task(
    summary: &Arc<Mutex<TerminalTaskEntry>>,
    killer: Arc<StdMutex<Box<dyn ChildKiller + Send + Sync>>>,
    io_control: Arc<PtyIoControl>,
    process_id: Option<u32>,
    capture_ledger: Arc<TerminalCaptureLedger>,
    cancel_requested: Arc<AtomicBool>,
    cancel_grace: Duration,
    artifacts: Arc<TerminalTaskArtifacts>,
    preview_limit_bytes: usize,
) -> Result<TerminalTaskEntry> {
    cancel_requested.store(true, Ordering::SeqCst);
    if let Some(process_id) = process_id {
        let _ = send_terminate_signal(process_id).await;
    }
    io_control.close();

    if let Some(entry) = wait_for_terminal_summary(summary, cancel_grace).await {
        return Ok(entry);
    }

    kill_pty_child(killer).await?;

    if let Some(entry) = wait_for_terminal_summary(summary, cancel_grace).await {
        Ok(entry)
    } else {
        let cleanup = cleanup_process_group_after_direct_exit(process_id).await;
        let status = if cleanup.status == ExecutionCleanupStatus::Completed {
            TerminalTaskStatus::Cancelled
        } else {
            TerminalTaskStatus::Interrupted
        };
        Ok(finalize_terminal_summary(
            summary,
            &artifacts,
            status,
            None,
            preview_limit_bytes,
            Some(cleanup),
            TerminalCaptureEvidence::from_ledger(&capture_ledger, None),
        )
        .await)
    }
}

pub(super) async fn wait_for_terminal_summary(
    summary: &Arc<Mutex<TerminalTaskEntry>>,
    max_wait: Duration,
) -> Option<TerminalTaskEntry> {
    let mut remaining = max_wait;
    let interval = Duration::from_millis(PTY_CANCEL_POLL_INTERVAL_MS);
    loop {
        let entry = summary.lock().await.clone();
        if entry.status.is_terminal() {
            return Some(entry);
        }
        if remaining.is_zero() {
            return None;
        }
        let delay = remaining.min(interval);
        sleep(delay).await;
        remaining = remaining.saturating_sub(delay);
    }
}

#[cfg(unix)]
pub(super) async fn send_terminate_signal(process_id: u32) -> Result<()> {
    if send_process_group_signal(process_id, "TERM").await.is_ok() {
        return Ok(());
    }

    let mut command = Command::new("kill");
    command.arg("-TERM").arg(process_id.to_string());
    let status =
        run_terminal_cleanup_command(command, format!("kill -TERM terminal process {process_id}"))
            .await?;
    if status.success() {
        Ok(())
    } else {
        bail!("kill returned non-zero status for terminal process {process_id}");
    }
}

#[cfg(unix)]
async fn wait_for_terminal_process_group_exit(process_id: u32) -> Result<bool> {
    let deadline = tokio::time::Instant::now() + TERMINAL_CLEANUP_WAIT_TIMEOUT;
    loop {
        if !process_group_has_live_members(process_id).await? {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        sleep(Duration::from_millis(10)).await;
    }
}

async fn run_terminal_cleanup_command(
    mut command: Command,
    description: impl Into<String>,
) -> Result<std::process::ExitStatus> {
    let description = description.into();
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match timeout(TERMINAL_CLEANUP_COMMAND_TIMEOUT, command.status()).await {
        Ok(status) => status.with_context(|| format!("failed to invoke {description}")),
        Err(_) => bail!(
            "{description} exceeded the {:?} cleanup-command deadline",
            TERMINAL_CLEANUP_COMMAND_TIMEOUT
        ),
    }
}

async fn kill_pty_child(killer: Arc<StdMutex<Box<dyn ChildKiller + Send + Sync>>>) -> Result<()> {
    let kill_task = task::spawn_blocking(move || -> Result<()> {
        let mut killer = killer
            .lock()
            .map_err(|_| anyhow!("terminal pty killer lock poisoned"))?;
        killer.kill().context("failed to kill terminal pty child")
    });
    timeout(TERMINAL_CLEANUP_WAIT_TIMEOUT, kill_task)
        .await
        .with_context(|| {
            format!(
                "terminal pty kill exceeded {:?}",
                TERMINAL_CLEANUP_WAIT_TIMEOUT
            )
        })?
        .context("terminal pty kill task failed")?
}

#[cfg(windows)]
pub(super) async fn send_terminate_signal(process_id: u32) -> Result<()> {
    terminate_owned_process_tree(process_id)
}

#[cfg(not(any(unix, windows)))]
pub(super) async fn send_terminate_signal(_process_id: u32) -> Result<()> {
    Ok(())
}
