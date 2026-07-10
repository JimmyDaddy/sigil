use super::*;

#[derive(Clone)]
pub struct TerminalProcessManager {
    workspace_root: PathBuf,
    artifact_root: PathBuf,
    artifact_label_root: PathBuf,
    terminal_execution: TerminalExecutionConfig,
    pub(super) tasks: Arc<Mutex<BTreeMap<TerminalTaskId, ManagedTerminalTask>>>,
    permission_contexts: Arc<StdMutex<BTreeMap<TerminalTaskId, TerminalTaskPermissionContext>>>,
    next_counter: Arc<AtomicU64>,
    preview_limit_bytes: usize,
    artifact_limits: TerminalArtifactLimits,
    cancel_grace: Duration,
}

impl TerminalProcessManager {
    /// Creates a non-PTY process manager rooted at `workspace_root`.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace root cannot be canonicalized.
    pub fn new(workspace_root: impl AsRef<Path>) -> Result<Self> {
        let workspace_root = canonical_workspace_root(workspace_root.as_ref())?;
        Self::new_with_artifact_root(
            &workspace_root,
            workspace_root.join(TERMINAL_TASK_ARTIFACT_ROOT),
            PathBuf::from(TERMINAL_TASK_ARTIFACT_ROOT),
        )
    }

    /// Creates a non-PTY process manager rooted at an injected artifact directory.
    ///
    /// `artifact_label_root` is stored in model-visible task metadata instead of the absolute
    /// machine-local artifact root.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace root cannot be canonicalized.
    pub fn new_with_artifact_root(
        workspace_root: impl AsRef<Path>,
        artifact_root: impl AsRef<Path>,
        artifact_label_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        Self::new_with_artifact_root_and_terminal_execution(
            workspace_root,
            artifact_root,
            artifact_label_root,
            TerminalExecutionConfig::default(),
        )
    }

    /// Creates a process manager with an injected terminal execution policy.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace root cannot be canonicalized.
    pub fn new_with_artifact_root_and_terminal_execution(
        workspace_root: impl AsRef<Path>,
        artifact_root: impl AsRef<Path>,
        artifact_label_root: impl Into<PathBuf>,
        terminal_execution: TerminalExecutionConfig,
    ) -> Result<Self> {
        let workspace_root = canonical_workspace_root(workspace_root.as_ref())?;
        Ok(Self {
            artifact_root: absolute_path_from(&workspace_root, artifact_root.as_ref()),
            artifact_label_root: artifact_label_root.into(),
            terminal_execution,
            workspace_root,
            tasks: Arc::new(Mutex::new(BTreeMap::new())),
            permission_contexts: Arc::new(StdMutex::new(BTreeMap::new())),
            next_counter: Arc::new(AtomicU64::new(1)),
            preview_limit_bytes: DEFAULT_TERMINAL_PREVIEW_LIMIT_BYTES,
            artifact_limits: TerminalArtifactLimits::default(),
            cancel_grace: Duration::from_millis(DEFAULT_CANCEL_GRACE_MS),
        })
    }

    pub fn with_preview_limit_bytes(mut self, preview_limit_bytes: usize) -> Self {
        self.preview_limit_bytes =
            preview_limit_bytes.clamp(1, DEFAULT_TERMINAL_PREVIEW_LIMIT_BYTES);
        self
    }

    #[cfg(test)]
    pub(super) fn with_artifact_limits(mut self, stream_bytes: u64, combined_bytes: u64) -> Self {
        self.artifact_limits = TerminalArtifactLimits {
            stream_bytes: stream_bytes.max(1),
            combined_bytes: combined_bytes.max(1),
        };
        self
    }

    pub fn with_cancel_grace(mut self, cancel_grace: Duration) -> Self {
        self.cancel_grace = cancel_grace;
        self
    }

    /// Starts one non-PTY background process and returns its initial durable task entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the command is empty, cwd escapes the workspace, artifacts cannot be
    /// created, the task id already exists, or process spawn fails.
    pub async fn start(&self, request: TerminalStartRequest) -> Result<TerminalTaskEntry> {
        let plan = self
            .prepare_start(request, TerminalStartMode::LocalProcess)
            .await?;

        let mut command_process = Command::new(&plan.shell);
        command_process
            .arg("-lc")
            .arg(&plan.command)
            .current_dir(&plan.resolved_cwd.absolute)
            .envs(&plan.env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        configure_process_group(&mut command_process);

        let mut child = command_process
            .spawn()
            .with_context(|| format!("failed to start terminal command: {}", plan.command))?;
        let process_id = child.id();
        let output_file = Arc::new(Mutex::new(CombinedOutputWriter::new(
            open_append_file(&plan.artifacts.absolute_output).await?,
            self.artifact_limits.combined_bytes,
        )));
        let capture_ledger = Arc::new(TerminalCaptureLedger::default());
        let (capture_failure_tx, capture_failure_rx) = mpsc::unbounded_channel();
        let stdout_task = spawn_capture_task(
            child.stdout.take(),
            TerminalOutputStream::Stdout,
            plan.artifacts.absolute_stdout.clone(),
            Arc::clone(&output_file),
            self.artifact_limits,
            Arc::clone(&capture_ledger),
            capture_failure_tx.clone(),
        );
        let stderr_task = spawn_capture_task(
            child.stderr.take(),
            TerminalOutputStream::Stderr,
            plan.artifacts.absolute_stderr.clone(),
            Arc::clone(&output_file),
            self.artifact_limits,
            Arc::clone(&capture_ledger),
            capture_failure_tx,
        );
        let summary = Arc::new(Mutex::new(plan.initial_entry.clone()));
        let (cancel_tx, cancel_rx) = mpsc::channel(1);
        let managed = ManagedTerminalTask {
            summary: Arc::clone(&summary),
            control: TerminalTaskControl::Process { cancel_tx },
        };

        self.tasks
            .lock()
            .await
            .insert(plan.task_id.clone(), managed);
        self.record_permission_context(&plan)?;
        tokio::spawn(run_terminal_worker(TerminalWorker {
            child,
            process_id,
            summary,
            artifacts: plan.artifacts,
            stdout_task,
            stderr_task,
            capture_ledger,
            capture_failure_rx,
            cancel_rx,
            preview_limit_bytes: self.preview_limit_bytes,
            cancel_grace: self.cancel_grace,
        }));

        Ok(plan.initial_entry)
    }

    /// Starts one PTY-backed background task and returns its initial durable task entry.
    ///
    /// # Errors
    ///
    /// Returns an error when task initialization fails or the platform PTY cannot spawn the
    /// requested command.
    pub async fn start_pty(
        &self,
        request: TerminalStartRequest,
        pty_size: Option<TerminalPtySize>,
    ) -> Result<TerminalTaskEntry> {
        let plan = self.prepare_start(request, TerminalStartMode::Pty).await?;
        let pty_runtime =
            spawn_pty_runtime(&plan, pty_size.unwrap_or_default(), self.artifact_limits)?;
        let summary = Arc::new(Mutex::new(plan.initial_entry.clone()));
        let managed = ManagedTerminalTask {
            summary: Arc::clone(&summary),
            control: TerminalTaskControl::Pty {
                input_tx: pty_runtime.input_tx.clone(),
                master: Arc::clone(&pty_runtime.master),
                killer: Arc::clone(&pty_runtime.killer),
                process_id: pty_runtime.process_id,
                capture_ledger: Arc::clone(&pty_runtime.capture_ledger),
                cancel_requested: Arc::clone(&pty_runtime.cancel_requested),
                cancel_grace: self.cancel_grace,
                artifacts: Arc::new(plan.artifacts.clone()),
                preview_limit_bytes: self.preview_limit_bytes,
            },
        };

        self.tasks
            .lock()
            .await
            .insert(plan.task_id.clone(), managed);
        self.record_permission_context(&plan)?;
        tokio::spawn(run_pty_worker(PtyWorker {
            summary,
            artifacts: plan.artifacts,
            wait_task: pty_runtime.wait_task,
            killer: pty_runtime.killer,
            process_id: pty_runtime.process_id,
            capture_ledger: pty_runtime.capture_ledger,
            cancel_requested: pty_runtime.cancel_requested,
            capture_failure_rx: pty_runtime.capture_failure_rx,
            child_exit_rx: pty_runtime.child_exit_rx,
            preview_limit_bytes: self.preview_limit_bytes,
            cancel_grace: self.cancel_grace,
        }));

        Ok(plan.initial_entry)
    }

    /// Returns the latest known task entry.
    ///
    /// # Errors
    ///
    /// Returns an error when `task_id` is not managed by this process manager.
    pub async fn status(&self, task_id: &TerminalTaskId) -> Result<TerminalTaskEntry> {
        let task = self.managed_task(task_id).await?;
        Ok(task.summary.lock().await.clone())
    }

    /// Reads a bounded slice of the combined output log.
    ///
    /// # Errors
    ///
    /// Returns an error when `task_id` is unknown or the artifact log cannot be read.
    pub async fn read(
        &self,
        task_id: &TerminalTaskId,
        offset: u64,
        limit_bytes: usize,
    ) -> Result<TerminalReadResult> {
        let task = self.managed_task(task_id).await?;
        let entry = task.summary.lock().await.clone();
        let path = self.stored_artifact_path(&entry.handle.log_path)?;
        let limit_bytes = limit_bytes.clamp(1, HARD_TERMINAL_READ_LIMIT_BYTES);
        let mut read =
            read_terminal_output_log(task_id.clone(), &path, offset, limit_bytes).await?;
        read.latest_entry = Some(entry);
        Ok(read)
    }

    /// Sends input to a PTY-backed terminal task.
    ///
    /// # Errors
    ///
    /// Returns an error when the task is unknown, not active, not PTY-backed, or stdin write fails.
    pub async fn input(
        &self,
        task_id: &TerminalTaskId,
        input: impl Into<String>,
    ) -> Result<TerminalInputResult> {
        let task = self.managed_task(task_id).await?;
        let current = task.summary.lock().await.clone();
        if current.status.is_terminal() {
            bail!("terminal task is not running: {}", task_id.as_str());
        }

        let TerminalTaskControl::Pty { input_tx, .. } = &task.control else {
            bail!("terminal task backend does not support input: process");
        };

        let input = input.into().into_bytes();
        if input.len() > MAX_TERMINAL_INPUT_BYTES {
            bail!(
                "terminal input exceeds maximum of {} bytes",
                MAX_TERMINAL_INPUT_BYTES
            );
        }
        let input_bytes = input.len() as u64;
        input_tx.try_send(input).map_err(|error| match error {
            std_mpsc::TrySendError::Full(_) => {
                anyhow!("terminal pty input queue is full: {}", task_id.as_str())
            }
            std_mpsc::TrySendError::Disconnected(_) => {
                anyhow!("terminal task is no longer running: {}", task_id.as_str())
            }
        })?;

        Ok(TerminalInputResult {
            task_id: task_id.clone(),
            input_bytes,
            backend: TerminalBackendKind::Pty,
        })
    }

    /// Resizes a PTY-backed terminal task.
    ///
    /// # Errors
    ///
    /// Returns an error when the task is unknown, not active, not PTY-backed, or resize fails.
    pub async fn resize(
        &self,
        task_id: &TerminalTaskId,
        size: TerminalPtySize,
    ) -> Result<TerminalResizeResult> {
        let task = self.managed_task(task_id).await?;
        let current = task.summary.lock().await.clone();
        if current.status.is_terminal() {
            bail!("terminal task is not running: {}", task_id.as_str());
        }

        let TerminalTaskControl::Pty { master, .. } = &task.control else {
            bail!("terminal task backend does not support resize: process");
        };

        let task_id_for_error = task_id.as_str().to_owned();
        let master = Arc::clone(master);
        task::spawn_blocking(move || -> Result<()> {
            let master = master
                .lock()
                .map_err(|_| anyhow!("terminal pty master lock poisoned: {task_id_for_error}"))?;
            master
                .resize(size.to_portable())
                .context("failed to resize terminal pty")
        })
        .await
        .context("terminal pty resize task failed")??;

        Ok(TerminalResizeResult {
            task_id: task_id.clone(),
            size,
            backend: TerminalBackendKind::Pty,
        })
    }

    /// Cancels a running task and returns the resulting latest entry.
    ///
    /// # Errors
    ///
    /// Returns an error when `task_id` is unknown or the cancel request cannot be sent.
    pub async fn cancel(&self, task_id: &TerminalTaskId) -> Result<TerminalTaskEntry> {
        let task = self.managed_task(task_id).await?;
        let current = task.summary.lock().await.clone();
        if current.status.is_terminal() {
            return Ok(current);
        }

        match &task.control {
            TerminalTaskControl::Process { cancel_tx } => {
                let (respond_to, response) = oneshot::channel();
                cancel_tx
                    .send(CancelCommand { respond_to })
                    .await
                    .map_err(|_| {
                        anyhow!("terminal task is no longer running: {}", task_id.as_str())
                    })?;
                match response.await {
                    Ok(entry) => Ok(entry),
                    Err(_) => {
                        let entry = task.summary.lock().await.clone();
                        if entry.status.is_terminal() {
                            Ok(entry)
                        } else {
                            Err(anyhow!(
                                "terminal cancellation response was lost before cleanup could be confirmed: {}",
                                task_id.as_str()
                            ))
                        }
                    }
                }
            }
            TerminalTaskControl::Pty {
                killer,
                process_id,
                capture_ledger,
                cancel_requested,
                cancel_grace,
                artifacts,
                preview_limit_bytes,
                ..
            } => {
                cancel_pty_task(
                    &task.summary,
                    Arc::clone(killer),
                    *process_id,
                    Arc::clone(capture_ledger),
                    Arc::clone(cancel_requested),
                    *cancel_grace,
                    artifacts.clone(),
                    *preview_limit_bytes,
                )
                .await
            }
        }
    }

    pub fn artifacts_for(&self, task_id: &TerminalTaskId) -> Result<TerminalTaskArtifacts> {
        let relative_dir = self.artifact_label_root.join(task_id.as_str());
        let relative_meta = relative_dir.join(TERMINAL_TASK_META_FILE);
        let relative_output = relative_dir.join(TERMINAL_TASK_OUTPUT_FILE);
        let relative_stdout = relative_dir.join(TERMINAL_TASK_STDOUT_FILE);
        let relative_stderr = relative_dir.join(TERMINAL_TASK_STDERR_FILE);
        let absolute_dir = self.artifact_root.join(task_id.as_str());
        Ok(TerminalTaskArtifacts {
            task_id: task_id.clone(),
            absolute_meta: absolute_dir.join(TERMINAL_TASK_META_FILE),
            absolute_output: absolute_dir.join(TERMINAL_TASK_OUTPUT_FILE),
            absolute_stdout: absolute_dir.join(TERMINAL_TASK_STDOUT_FILE),
            absolute_stderr: absolute_dir.join(TERMINAL_TASK_STDERR_FILE),
            absolute_dir,
            relative_dir,
            relative_meta,
            relative_output,
            relative_stdout,
            relative_stderr,
        })
    }

    /// Returns the permission context for a live terminal task.
    ///
    /// # Errors
    ///
    /// Returns an error when the task is not known to this process manager.
    pub fn permission_context(
        &self,
        task_id: &TerminalTaskId,
    ) -> Result<TerminalTaskPermissionContext> {
        self.permission_contexts
            .lock()
            .map_err(|_| anyhow!("terminal permission context lock poisoned"))?
            .get(task_id)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "terminal task permission context is unavailable: {}",
                    task_id.as_str()
                )
            })
    }

    async fn managed_task(&self, task_id: &TerminalTaskId) -> Result<ManagedTerminalTask> {
        self.tasks
            .lock()
            .await
            .get(task_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown terminal task: {}", task_id.as_str()))
    }

    pub(super) fn stored_artifact_path(&self, relative_path: &Path) -> Result<PathBuf> {
        if relative_path.is_absolute() {
            bail!(
                "terminal artifact path must be relative: {}",
                relative_path.display()
            );
        }
        let suffix = relative_path
            .strip_prefix(&self.artifact_label_root)
            .map_err(|_| {
                anyhow!(
                    "terminal artifact path has unknown label: {}",
                    relative_path.display()
                )
            })?;
        let lexical = lexically_normalize_path(&self.artifact_root.join(suffix))?;
        let resolved_prefix = resolve_existing_prefix(&lexical)?;
        if !resolved_prefix.starts_with(&self.artifact_root)
            && !resolved_prefix.starts_with(&self.workspace_root)
        {
            bail!(
                "terminal artifact path is outside artifact root: {}",
                relative_path.display()
            );
        }
        Ok(lexical)
    }

    fn next_task_id(&self, created_at_ms: u64) -> Result<TerminalTaskId> {
        let counter = self.next_counter.fetch_add(1, Ordering::Relaxed);
        TerminalTaskId::new(format!("terminal-{created_at_ms}-{counter}"))
    }

    fn record_permission_context(&self, plan: &TerminalTaskStartPlan) -> Result<()> {
        self.permission_contexts
            .lock()
            .map_err(|_| anyhow!("terminal permission context lock poisoned"))?
            .insert(
                plan.task_id.clone(),
                TerminalTaskPermissionContext {
                    task_id: plan.task_id.clone(),
                    command: plan.command.clone(),
                    cwd: plan.resolved_cwd.absolute.clone(),
                    shell: plan.shell.clone(),
                },
            );
        Ok(())
    }

    async fn prepare_start(
        &self,
        request: TerminalStartRequest,
        mode: TerminalStartMode,
    ) -> Result<TerminalTaskStartPlan> {
        let command = request.command.trim().to_owned();
        if command.is_empty() {
            bail!("terminal command cannot be empty");
        }

        let created_at_ms = current_epoch_ms();
        let task_id = match request.task_id {
            Some(task_id) => task_id,
            None => self.next_task_id(created_at_ms)?,
        };
        let artifacts = self.artifacts_for(&task_id)?;
        let resolved_cwd = resolve_terminal_cwd(&self.workspace_root, request.cwd.as_deref())?;
        let shell = request.shell.unwrap_or_else(|| "sh".to_owned());
        let env = request.env;
        let execution = match mode {
            TerminalStartMode::LocalProcess => local_process_execution(),
            TerminalStartMode::Pty => self
                .terminal_execution
                .resolve_pty_execution(&resolved_cwd.absolute, &shell, &command, &env)?
                .into(),
        };

        {
            let tasks = self.tasks.lock().await;
            if tasks.contains_key(&task_id) {
                bail!("terminal task already exists: {}", task_id.as_str());
            }
        }

        fs::create_dir_all(&artifacts.absolute_dir)
            .await
            .with_context(|| format!("failed to create {}", artifacts.absolute_dir.display()))?;
        create_empty_log_files(&artifacts).await?;

        let handle = TerminalTaskHandle {
            task_id: task_id.clone(),
            command: command.clone(),
            cwd: resolved_cwd.relative.clone(),
            shell: shell.clone(),
            log_path: artifacts.relative_output.clone(),
            created_at_ms,
            execution_backend: Some(execution.execution_backend),
            execution_backend_capabilities: Some(execution.execution_backend_capabilities),
            enforcement_backend: Some(execution.enforcement_backend),
            enforcement_backend_capabilities: Some(execution.enforcement_backend_capabilities),
            sandbox_profile: Some(execution.sandbox_profile),
        };
        let initial_entry = TerminalTaskEntry {
            handle: handle.clone(),
            status: TerminalTaskStatus::Running,
            output_preview: None,
            output_hash: None,
            output_truncated: false,
            output_total_bytes: 0,
            output_limit_bytes: None,
            output_termination_reason: None,
            cleanup: None,
            updated_at_ms: created_at_ms,
        };
        write_task_meta(&artifacts.absolute_meta, &initial_entry).await?;

        Ok(TerminalTaskStartPlan {
            task_id,
            command,
            shell,
            env,
            artifacts,
            resolved_cwd,
            initial_entry,
            pty_command: execution.pty_command,
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum TerminalStartMode {
    LocalProcess,
    Pty,
}

struct TerminalStartExecution {
    execution_backend: TerminalExecutionBackendKind,
    execution_backend_capabilities: TerminalExecutionBackendCapabilities,
    enforcement_backend: ExecutionBackendKind,
    enforcement_backend_capabilities: ExecutionBackendCapabilities,
    sandbox_profile: ExecutionSandboxProfile,
    pty_command: Option<TerminalPtyCommandSpec>,
}

fn local_process_execution() -> TerminalStartExecution {
    TerminalStartExecution {
        execution_backend: TerminalExecutionBackendKind::LocalProcess,
        execution_backend_capabilities: TerminalExecutionBackendCapabilities::local_process(),
        enforcement_backend: ExecutionBackendKind::Local,
        enforcement_backend_capabilities: ExecutionBackendCapabilities::default(),
        sandbox_profile: ExecutionSandboxProfile::Unconfined,
        pty_command: None,
    }
}

impl From<TerminalPtyExecution> for TerminalStartExecution {
    fn from(execution: TerminalPtyExecution) -> Self {
        Self {
            execution_backend: execution.execution_backend,
            execution_backend_capabilities: execution.execution_backend_capabilities,
            enforcement_backend: execution.enforcement_backend,
            enforcement_backend_capabilities: execution.enforcement_backend_capabilities,
            sandbox_profile: execution.sandbox_profile,
            pty_command: Some(execution.command),
        }
    }
}

#[derive(Clone)]
pub(super) struct ManagedTerminalTask {
    pub(super) summary: Arc<Mutex<TerminalTaskEntry>>,
    pub(super) control: TerminalTaskControl,
}

#[derive(Clone)]
pub(super) enum TerminalTaskControl {
    Process {
        cancel_tx: mpsc::Sender<CancelCommand>,
    },
    Pty {
        input_tx: std_mpsc::SyncSender<Vec<u8>>,
        master: Arc<StdMutex<Box<dyn MasterPty + Send>>>,
        killer: Arc<StdMutex<Box<dyn ChildKiller + Send + Sync>>>,
        process_id: Option<u32>,
        capture_ledger: Arc<TerminalCaptureLedger>,
        cancel_requested: Arc<AtomicBool>,
        cancel_grace: Duration,
        artifacts: Arc<TerminalTaskArtifacts>,
        preview_limit_bytes: usize,
    },
}

pub(super) struct CancelCommand {
    pub(super) respond_to: oneshot::Sender<TerminalTaskEntry>,
}

pub(super) struct TerminalTaskStartPlan {
    pub(super) task_id: TerminalTaskId,
    pub(super) command: String,
    pub(super) shell: String,
    pub(super) env: BTreeMap<String, String>,
    pub(super) artifacts: TerminalTaskArtifacts,
    pub(super) resolved_cwd: ResolvedTerminalCwd,
    pub(super) initial_entry: TerminalTaskEntry,
    pub(super) pty_command: Option<TerminalPtyCommandSpec>,
}
