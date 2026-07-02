use super::*;

pub(super) struct PtyRuntime {
    pub(super) input_tx: std_mpsc::SyncSender<Vec<u8>>,
    pub(super) master: Arc<StdMutex<Box<dyn MasterPty + Send>>>,
    pub(super) killer: Arc<StdMutex<Box<dyn ChildKiller + Send + Sync>>>,
    pub(super) process_id: Option<u32>,
    pub(super) cancel_requested: Arc<AtomicBool>,
    pub(super) wait_task: JoinHandle<PtyWaitOutcome>,
}

pub(super) struct PtyWaitOutcome {
    pub(super) status: TerminalTaskStatus,
    pub(super) capture_error: Option<String>,
}

pub(super) struct TerminalWorker {
    pub(super) child: TokioChild,
    pub(super) process_id: Option<u32>,
    pub(super) summary: Arc<Mutex<TerminalTaskEntry>>,
    pub(super) artifacts: TerminalTaskArtifacts,
    pub(super) stdout_task: JoinHandle<Result<u64>>,
    pub(super) stderr_task: JoinHandle<Result<u64>>,
    pub(super) cancel_rx: mpsc::Receiver<CancelCommand>,
    pub(super) preview_limit_bytes: usize,
    pub(super) cancel_grace: Duration,
}

pub(super) struct PtyWorker {
    pub(super) summary: Arc<Mutex<TerminalTaskEntry>>,
    pub(super) artifacts: TerminalTaskArtifacts,
    pub(super) wait_task: JoinHandle<PtyWaitOutcome>,
    pub(super) preview_limit_bytes: usize,
}

pub(super) fn spawn_pty_runtime(
    plan: &TerminalTaskStartPlan,
    size: TerminalPtySize,
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
    let master = Arc::new(StdMutex::new(master));
    let (input_tx, input_rx) = std_mpsc::sync_channel(TERMINAL_PTY_INPUT_QUEUE_BOUND);
    spawn_pty_input_thread(writer, input_rx);

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
    let killer = Arc::new(StdMutex::new(child.clone_killer()));
    let cancel_requested = Arc::new(AtomicBool::new(false));
    let wait_cancel_requested = Arc::clone(&cancel_requested);
    let read_thread = spawn_pty_read_thread(
        reader,
        plan.artifacts.absolute_stdout.clone(),
        plan.artifacts.absolute_output.clone(),
    );
    let wait_task = task::spawn_blocking(move || {
        let wait_result = child.wait();
        let capture_error = join_pty_read_thread(read_thread);
        let status = if wait_cancel_requested.load(Ordering::SeqCst) {
            TerminalTaskStatus::Cancelled
        } else {
            status_from_pty_wait_result(wait_result)
        };
        PtyWaitOutcome {
            status,
            capture_error,
        }
    });

    Ok(PtyRuntime {
        input_tx,
        master,
        killer,
        process_id,
        cancel_requested,
        wait_task,
    })
}

pub(super) async fn run_terminal_worker(mut worker: TerminalWorker) {
    let final_status = tokio::select! {
        wait_result = worker.child.wait() => status_from_wait_result(wait_result),
        cancel = worker.cancel_rx.recv() => {
            if let Some(cancel) = cancel {
                let status = cancel_child(&mut worker.child, worker.process_id, worker.cancel_grace).await;
                let entry = finalize_terminal_task(&worker.summary, &worker.artifacts, status, worker.stdout_task, worker.stderr_task, worker.preview_limit_bytes).await;
                let _ = cancel.respond_to.send(entry);
                return;
            }
            status_from_wait_result(worker.child.wait().await)
        }
    };

    let _ = finalize_terminal_task(
        &worker.summary,
        &worker.artifacts,
        final_status,
        worker.stdout_task,
        worker.stderr_task,
        worker.preview_limit_bytes,
    )
    .await;
}

pub(super) async fn run_pty_worker(worker: PtyWorker) {
    let outcome = match worker.wait_task.await {
        Ok(outcome) => outcome,
        Err(error) => PtyWaitOutcome {
            status: TerminalTaskStatus::Failed {
                reason: format!("terminal pty wait task failed: {error}"),
            },
            capture_error: None,
        },
    };
    let _ = finalize_terminal_summary(
        &worker.summary,
        &worker.artifacts,
        outcome.status,
        outcome.capture_error,
        worker.preview_limit_bytes,
    )
    .await;
}

pub(super) async fn finalize_terminal_task(
    summary: &Arc<Mutex<TerminalTaskEntry>>,
    artifacts: &TerminalTaskArtifacts,
    status: TerminalTaskStatus,
    stdout_task: JoinHandle<Result<u64>>,
    stderr_task: JoinHandle<Result<u64>>,
    preview_limit_bytes: usize,
) -> TerminalTaskEntry {
    let stdout_result = join_capture_task(stdout_task).await;
    let stderr_result = join_capture_task(stderr_task).await;
    let capture_error = match (stdout_result, stderr_result) {
        (Err(error), _) | (_, Err(error)) => Some(error.to_string()),
        (Ok(_), Ok(_)) => None,
    };

    finalize_terminal_summary(
        summary,
        artifacts,
        status,
        capture_error,
        preview_limit_bytes,
    )
    .await
}

async fn finalize_terminal_summary(
    summary: &Arc<Mutex<TerminalTaskEntry>>,
    artifacts: &TerminalTaskArtifacts,
    status: TerminalTaskStatus,
    capture_error: Option<String>,
    preview_limit_bytes: usize,
) -> TerminalTaskEntry {
    let mut final_status = status;
    if let Some(error) = capture_error
        && matches!(final_status, TerminalTaskStatus::Exited { .. })
    {
        final_status = TerminalTaskStatus::Failed {
            reason: format!("failed to capture terminal output: {error}"),
        };
    }

    let log_summary = summarize_log(&artifacts.absolute_output, preview_limit_bytes)
        .await
        .unwrap_or_else(|error| LogSummary {
            preview: format!("failed to summarize terminal output: {error}"),
            sha256: String::new(),
            truncated: false,
        });

    let mut entry = summary.lock().await;
    entry.status = final_status;
    entry.output_preview = (!log_summary.preview.is_empty()).then_some(log_summary.preview);
    entry.output_hash = (!log_summary.sha256.is_empty()).then_some(log_summary.sha256);
    entry.output_truncated = log_summary.truncated;
    entry.cleanup = terminal_cleanup_receipt_for_status(&entry.status);
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

pub(super) async fn cancel_child(
    child: &mut TokioChild,
    process_id: Option<u32>,
    cancel_grace: Duration,
) -> TerminalTaskStatus {
    if let Some(process_id) = process_id {
        let _ = send_terminate_signal(process_id).await;
    }

    match timeout(cancel_grace, child.wait()).await {
        Ok(Ok(_)) => TerminalTaskStatus::Cancelled,
        Ok(Err(error)) => TerminalTaskStatus::Failed {
            reason: format!("terminal process cancel wait failed: {error}"),
        },
        Err(_) => match child.start_kill() {
            Ok(()) => match child.wait().await {
                Ok(_) => TerminalTaskStatus::Cancelled,
                Err(error) => TerminalTaskStatus::Failed {
                    reason: format!("terminal process kill wait failed: {error}"),
                },
            },
            Err(error) => TerminalTaskStatus::Failed {
                reason: format!("failed to kill terminal process: {error}"),
            },
        },
    }
}

pub(super) async fn cancel_pty_task(
    summary: &Arc<Mutex<TerminalTaskEntry>>,
    killer: Arc<StdMutex<Box<dyn ChildKiller + Send + Sync>>>,
    process_id: Option<u32>,
    cancel_requested: Arc<AtomicBool>,
    cancel_grace: Duration,
    artifacts: Arc<TerminalTaskArtifacts>,
    preview_limit_bytes: usize,
) -> Result<TerminalTaskEntry> {
    cancel_requested.store(true, Ordering::SeqCst);
    if let Some(process_id) = process_id {
        let _ = send_terminate_signal(process_id).await;
    }

    if let Some(entry) = wait_for_terminal_summary(summary, cancel_grace).await {
        return Ok(entry);
    }

    task::spawn_blocking(move || -> Result<()> {
        let mut killer = killer
            .lock()
            .map_err(|_| anyhow!("terminal pty killer lock poisoned"))?;
        killer.kill().context("failed to kill terminal pty child")
    })
    .await
    .context("terminal pty kill task failed")??;

    if let Some(entry) = wait_for_terminal_summary(summary, cancel_grace).await {
        Ok(entry)
    } else {
        Ok(finalize_terminal_summary(
            summary,
            &artifacts,
            TerminalTaskStatus::Cancelled,
            None,
            preview_limit_bytes,
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
    let group_target = format!("-{process_id}");
    let group_status = Command::new("kill")
        .arg("-TERM")
        .arg(&group_target)
        .stderr(Stdio::null())
        .status()
        .await
        .context("failed to invoke kill for terminal process group")?;
    if group_status.success() {
        return Ok(());
    }

    let status = Command::new("kill")
        .arg("-TERM")
        .arg(process_id.to_string())
        .stderr(Stdio::null())
        .status()
        .await
        .context("failed to invoke kill for terminal process")?;
    if status.success() {
        Ok(())
    } else {
        bail!("kill returned non-zero status for terminal process {process_id}");
    }
}

#[cfg(not(unix))]
pub(super) async fn send_terminate_signal(_process_id: u32) -> Result<()> {
    Ok(())
}
