use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigil_kernel::{TerminalTaskEntry, TerminalTaskHandle, TerminalTaskId, TerminalTaskStatus};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom},
    process::{Child, Command},
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
    time::{Duration, timeout},
};

const TERMINAL_TASK_ARTIFACT_ROOT: &str = ".sigil/tasks";
const TERMINAL_TASK_META_FILE: &str = "meta.json";
const TERMINAL_TASK_OUTPUT_FILE: &str = "output.log";
const TERMINAL_TASK_STDOUT_FILE: &str = "stdout.log";
const TERMINAL_TASK_STDERR_FILE: &str = "stderr.log";
const DEFAULT_TERMINAL_PREVIEW_LIMIT_BYTES: usize = 16 * 1024;
const DEFAULT_CANCEL_GRACE_MS: u64 = 500;

/// Request used by the non-PTY terminal process backend.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalStartRequest {
    pub task_id: Option<TerminalTaskId>,
    pub command: String,
    pub cwd: Option<PathBuf>,
    pub shell: Option<String>,
}

impl TerminalStartRequest {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            ..Self::default()
        }
    }
}

/// Workspace-relative and absolute artifact paths for one terminal task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TerminalTaskArtifacts {
    pub task_id: TerminalTaskId,
    pub relative_dir: PathBuf,
    pub relative_meta: PathBuf,
    pub relative_output: PathBuf,
    pub relative_stdout: PathBuf,
    pub relative_stderr: PathBuf,
    #[serde(skip)]
    pub absolute_dir: PathBuf,
    #[serde(skip)]
    pub absolute_meta: PathBuf,
    #[serde(skip)]
    pub absolute_output: PathBuf,
    #[serde(skip)]
    pub absolute_stdout: PathBuf,
    #[serde(skip)]
    pub absolute_stderr: PathBuf,
}

/// Bounded read result for a terminal task output log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TerminalReadResult {
    pub task_id: TerminalTaskId,
    pub offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u64>,
    pub content: String,
    pub returned_bytes: u64,
    pub total_bytes: u64,
    pub truncated: bool,
}

#[derive(Clone)]
pub struct TerminalProcessManager {
    workspace_root: PathBuf,
    tasks: Arc<Mutex<BTreeMap<TerminalTaskId, ManagedTerminalTask>>>,
    next_counter: Arc<AtomicU64>,
    preview_limit_bytes: usize,
    cancel_grace: Duration,
}

impl TerminalProcessManager {
    /// Creates a non-PTY process manager rooted at `workspace_root`.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace root cannot be canonicalized.
    pub fn new(workspace_root: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            workspace_root: canonical_workspace_root(workspace_root.as_ref())?,
            tasks: Arc::new(Mutex::new(BTreeMap::new())),
            next_counter: Arc::new(AtomicU64::new(1)),
            preview_limit_bytes: DEFAULT_TERMINAL_PREVIEW_LIMIT_BYTES,
            cancel_grace: Duration::from_millis(DEFAULT_CANCEL_GRACE_MS),
        })
    }

    pub fn with_preview_limit_bytes(mut self, preview_limit_bytes: usize) -> Self {
        self.preview_limit_bytes = preview_limit_bytes.max(1);
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
        };
        let initial_entry = TerminalTaskEntry {
            handle: handle.clone(),
            status: TerminalTaskStatus::Running,
            output_preview: None,
            output_hash: None,
            output_truncated: false,
            updated_at_ms: created_at_ms,
        };
        write_task_meta(&artifacts.absolute_meta, &initial_entry).await?;

        let mut command_process = Command::new(&shell);
        command_process
            .arg("-lc")
            .arg(&command)
            .current_dir(&resolved_cwd.absolute)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        configure_process_group(&mut command_process);

        let mut child = command_process
            .spawn()
            .with_context(|| format!("failed to start terminal command: {command}"))?;
        let process_id = child.id();
        let output_file = Arc::new(Mutex::new(
            open_append_file(&artifacts.absolute_output).await?,
        ));
        let stdout_task = spawn_capture_task(
            child.stdout.take(),
            artifacts.absolute_stdout.clone(),
            Arc::clone(&output_file),
        );
        let stderr_task = spawn_capture_task(
            child.stderr.take(),
            artifacts.absolute_stderr.clone(),
            Arc::clone(&output_file),
        );
        let summary = Arc::new(Mutex::new(initial_entry.clone()));
        let (cancel_tx, cancel_rx) = mpsc::channel(1);
        let managed = ManagedTerminalTask {
            summary: Arc::clone(&summary),
            cancel_tx,
        };

        self.tasks.lock().await.insert(task_id, managed);
        tokio::spawn(run_terminal_worker(TerminalWorker {
            child,
            process_id,
            summary,
            artifacts,
            stdout_task,
            stderr_task,
            cancel_rx,
            preview_limit_bytes: self.preview_limit_bytes,
            cancel_grace: self.cancel_grace,
        }));

        Ok(initial_entry)
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
        let path = self.workspace_artifact_path(&entry.handle.log_path)?;
        read_terminal_output_log(task_id.clone(), &path, offset, limit_bytes.max(1)).await
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

        let (respond_to, response) = oneshot::channel();
        task.cancel_tx
            .send(CancelCommand { respond_to })
            .await
            .map_err(|_| anyhow!("terminal task is no longer running: {}", task_id.as_str()))?;
        match response.await {
            Ok(entry) => Ok(entry),
            Err(_) => Ok(task.summary.lock().await.clone()),
        }
    }

    pub fn artifacts_for(&self, task_id: &TerminalTaskId) -> Result<TerminalTaskArtifacts> {
        let relative_dir = PathBuf::from(TERMINAL_TASK_ARTIFACT_ROOT).join(task_id.as_str());
        let relative_meta = relative_dir.join(TERMINAL_TASK_META_FILE);
        let relative_output = relative_dir.join(TERMINAL_TASK_OUTPUT_FILE);
        let relative_stdout = relative_dir.join(TERMINAL_TASK_STDOUT_FILE);
        let relative_stderr = relative_dir.join(TERMINAL_TASK_STDERR_FILE);
        Ok(TerminalTaskArtifacts {
            task_id: task_id.clone(),
            absolute_dir: self.workspace_artifact_path(&relative_dir)?,
            absolute_meta: self.workspace_artifact_path(&relative_meta)?,
            absolute_output: self.workspace_artifact_path(&relative_output)?,
            absolute_stdout: self.workspace_artifact_path(&relative_stdout)?,
            absolute_stderr: self.workspace_artifact_path(&relative_stderr)?,
            relative_dir,
            relative_meta,
            relative_output,
            relative_stdout,
            relative_stderr,
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

    fn workspace_artifact_path(&self, relative_path: &Path) -> Result<PathBuf> {
        if relative_path.is_absolute() {
            bail!(
                "terminal artifact path must be workspace-relative: {}",
                relative_path.display()
            );
        }
        let lexical = lexically_normalize_path(&self.workspace_root.join(relative_path))?;
        let resolved_prefix = resolve_existing_prefix(&lexical)?;
        if !resolved_prefix.starts_with(&self.workspace_root) {
            bail!(
                "terminal artifact path is outside workspace: {}",
                relative_path.display()
            );
        }
        Ok(lexical)
    }

    fn next_task_id(&self, created_at_ms: u64) -> Result<TerminalTaskId> {
        let counter = self.next_counter.fetch_add(1, Ordering::Relaxed);
        TerminalTaskId::new(format!("terminal-{created_at_ms}-{counter}"))
    }
}

#[derive(Clone)]
struct ManagedTerminalTask {
    summary: Arc<Mutex<TerminalTaskEntry>>,
    cancel_tx: mpsc::Sender<CancelCommand>,
}

struct CancelCommand {
    respond_to: oneshot::Sender<TerminalTaskEntry>,
}

struct TerminalWorker {
    child: Child,
    process_id: Option<u32>,
    summary: Arc<Mutex<TerminalTaskEntry>>,
    artifacts: TerminalTaskArtifacts,
    stdout_task: JoinHandle<Result<u64>>,
    stderr_task: JoinHandle<Result<u64>>,
    cancel_rx: mpsc::Receiver<CancelCommand>,
    preview_limit_bytes: usize,
    cancel_grace: Duration,
}

#[derive(Debug, Clone)]
struct ResolvedTerminalCwd {
    relative: PathBuf,
    absolute: PathBuf,
}

#[derive(Debug, Clone)]
struct LogSummary {
    preview: String,
    sha256: String,
    truncated: bool,
}

async fn run_terminal_worker(mut worker: TerminalWorker) {
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

async fn finalize_terminal_task(
    summary: &Arc<Mutex<TerminalTaskEntry>>,
    artifacts: &TerminalTaskArtifacts,
    status: TerminalTaskStatus,
    stdout_task: JoinHandle<Result<u64>>,
    stderr_task: JoinHandle<Result<u64>>,
    preview_limit_bytes: usize,
) -> TerminalTaskEntry {
    let stdout_result = join_capture_task(stdout_task).await;
    let stderr_result = join_capture_task(stderr_task).await;
    let mut final_status = status;
    if let Err(error) = stdout_result.as_ref().and(stderr_result.as_ref())
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
    entry.updated_at_ms = current_epoch_ms();
    let cloned = entry.clone();
    drop(entry);
    let _ = write_task_meta(&artifacts.absolute_meta, &cloned).await;
    cloned
}

fn status_from_wait_result(
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

async fn cancel_child(
    child: &mut Child,
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

#[cfg(unix)]
async fn send_terminate_signal(process_id: u32) -> Result<()> {
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
async fn send_terminate_signal(_process_id: u32) -> Result<()> {
    Ok(())
}

fn spawn_capture_task<R>(
    reader: Option<R>,
    stream_path: PathBuf,
    output_file: Arc<Mutex<File>>,
) -> JoinHandle<Result<u64>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(capture_stream(reader, stream_path, output_file))
}

async fn capture_stream<R>(
    mut reader: Option<R>,
    stream_path: PathBuf,
    output_file: Arc<Mutex<File>>,
) -> Result<u64>
where
    R: AsyncRead + Unpin,
{
    let mut stream_file = open_append_file(&stream_path).await?;
    let Some(reader) = reader.as_mut() else {
        return Ok(0);
    };
    let mut total = 0u64;
    let mut buffer = vec![0u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await.with_context(|| {
            format!(
                "failed to read terminal stream for {}",
                stream_path.display()
            )
        })?;
        if read == 0 {
            break;
        }
        stream_file
            .write_all(&buffer[..read])
            .await
            .with_context(|| format!("failed to write {}", stream_path.display()))?;
        let mut combined = output_file.lock().await;
        combined
            .write_all(&buffer[..read])
            .await
            .context("failed to write terminal combined output log")?;
        total += read as u64;
    }
    stream_file
        .flush()
        .await
        .with_context(|| format!("failed to flush {}", stream_path.display()))?;
    output_file
        .lock()
        .await
        .flush()
        .await
        .context("failed to flush terminal combined output log")?;
    Ok(total)
}

async fn join_capture_task(task: JoinHandle<Result<u64>>) -> Result<u64> {
    match task.await {
        Ok(result) => result,
        Err(error) => Err(anyhow!("terminal capture task failed: {error}")),
    }
}

async fn read_terminal_output_log(
    task_id: TerminalTaskId,
    path: &Path,
    offset: u64,
    limit_bytes: usize,
) -> Result<TerminalReadResult> {
    let mut file = File::open(path)
        .await
        .with_context(|| format!("failed to open {}", path.display()))?;
    let total_bytes = file
        .metadata()
        .await
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .len();
    let start = offset.min(total_bytes);
    file.seek(SeekFrom::Start(start))
        .await
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let max_len = (total_bytes - start).min(limit_bytes as u64) as usize;
    let mut buffer = vec![0u8; max_len];
    if max_len > 0 {
        file.read_exact(&mut buffer)
            .await
            .with_context(|| format!("failed to read {}", path.display()))?;
    }
    let returned_bytes = buffer.len() as u64;
    let next_offset = start + returned_bytes;
    Ok(TerminalReadResult {
        task_id,
        offset: start,
        next_offset: (next_offset < total_bytes).then_some(next_offset),
        content: String::from_utf8_lossy(&buffer).to_string(),
        returned_bytes,
        total_bytes,
        truncated: next_offset < total_bytes,
    })
}

async fn summarize_log(path: &Path, limit_bytes: usize) -> Result<LogSummary> {
    let bytes = fs::read(path)
        .await
        .with_context(|| format!("failed to read {}", path.display()))?;
    let sha256 = sha256_hex(&bytes);
    let limited = limit_output_bytes(&bytes, limit_bytes.max(1));
    Ok(LogSummary {
        preview: limited.content,
        sha256,
        truncated: limited.truncated,
    })
}

#[derive(Debug, Clone)]
struct LimitedOutput {
    content: String,
    truncated: bool,
}

fn limit_output_bytes(bytes: &[u8], limit_bytes: usize) -> LimitedOutput {
    if bytes.len() <= limit_bytes {
        return LimitedOutput {
            content: String::from_utf8_lossy(bytes).to_string(),
            truncated: false,
        };
    }

    let head_len = limit_bytes / 2;
    let tail_len = limit_bytes.saturating_sub(head_len);
    let omitted = bytes.len().saturating_sub(head_len + tail_len);
    let mut content = String::new();
    content.push_str(&String::from_utf8_lossy(&bytes[..head_len]));
    content.push_str(&format!("\n... truncated {omitted} bytes ...\n"));
    content.push_str(&String::from_utf8_lossy(&bytes[bytes.len() - tail_len..]));
    LimitedOutput {
        content,
        truncated: true,
    }
}

async fn create_empty_log_files(artifacts: &TerminalTaskArtifacts) -> Result<()> {
    for path in [
        &artifacts.absolute_output,
        &artifacts.absolute_stdout,
        &artifacts.absolute_stderr,
    ] {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .await
            .with_context(|| format!("failed to create {}", path.display()))?;
    }
    Ok(())
}

async fn open_append_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .with_context(|| format!("failed to open {}", path.display()))
}

async fn write_task_meta(path: &Path, entry: &TerminalTaskEntry) -> Result<()> {
    let bytes =
        serde_json::to_vec_pretty(entry).context("failed to serialize terminal task meta")?;
    fs::write(path, bytes)
        .await
        .with_context(|| format!("failed to write {}", path.display()))
}

fn resolve_terminal_cwd(
    workspace_root: &Path,
    requested: Option<&Path>,
) -> Result<ResolvedTerminalCwd> {
    let requested = requested.unwrap_or_else(|| Path::new("."));
    if requested.as_os_str().is_empty() {
        bail!("terminal cwd cannot be empty");
    }
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace_root.join(requested)
    };
    let lexical = lexically_normalize_path(&candidate)?;
    let canonical = canonical_workspace_root(&lexical)?;
    if !canonical.starts_with(workspace_root) {
        bail!("terminal cwd is outside workspace: {}", requested.display());
    }
    let relative = if canonical == workspace_root {
        PathBuf::from(".")
    } else {
        canonical
            .strip_prefix(workspace_root)
            .unwrap_or(&canonical)
            .to_path_buf()
    };
    Ok(ResolvedTerminalCwd {
        relative,
        absolute: canonical,
    })
}

fn canonical_workspace_root(workspace_root: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(workspace_root).with_context(|| {
        format!(
            "failed to resolve workspace root {}",
            workspace_root.display()
        )
    })
}

fn lexically_normalize_path(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) => bail!("platform path prefixes are not supported"),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        Ok(PathBuf::from("."))
    } else {
        Ok(normalized)
    }
}

fn resolve_existing_prefix(absolute_path: &Path) -> Result<PathBuf> {
    let mut resolved = PathBuf::new();
    for (index, component) in absolute_path.components().enumerate() {
        let candidate = if resolved.as_os_str().is_empty() {
            PathBuf::from(component.as_os_str())
        } else {
            resolved.join(component.as_os_str())
        };
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => {
                resolved = std::fs::canonicalize(&candidate)
                    .with_context(|| format!("failed to resolve {}", candidate.display()))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut missing_path = candidate;
                for remaining in absolute_path.components().skip(index + 1) {
                    missing_path.push(remaining.as_os_str());
                }
                return lexically_normalize_path(&missing_path);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", candidate.display()));
            }
        }
    }
    Ok(resolved)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(test)]
#[path = "tests/terminal_process_tests.rs"]
mod tests;
