use std::{
    fmt, io, net::SocketAddr, path::PathBuf, process::ExitStatus, sync::Arc, time::Duration,
};

use reqwest::{Client, redirect::Policy};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, ChildStdin, Command},
    task::JoinHandle,
    time::{Instant, timeout, timeout_at},
};

use crate::{client::DesktopHttpClient, protocol::DesktopServerInfo, secret::DesktopBearerToken};

const MAX_BOOTSTRAP_BYTES: usize = 16 * 1024;
const MAX_SERVER_INFO_BYTES: usize = 16 * 1024;
const FAILED_LAUNCH_GRACE: Duration = Duration::from_millis(250);
const FORCED_REAP_TIMEOUT: Duration = Duration::from_secs(5);
const PIPE_TASK_FINISH_TIMEOUT: Duration = Duration::from_millis(250);

/// Exact local inputs needed to launch one workspace-owned server process.
#[derive(Clone)]
pub struct DesktopLaunchRequest {
    /// Path to the `sigil` binary bundled with or selected by the native shell.
    pub sigil_binary: PathBuf,
    /// Explicit configuration loaded by the server child, if one was selected.
    pub config_path: Option<PathBuf>,
    /// Workspace root used as the child working directory.
    pub workspace_root: PathBuf,
}

impl DesktopLaunchRequest {
    /// Creates a launch request with an explicit configuration path.
    #[must_use]
    pub fn new(
        sigil_binary: impl Into<PathBuf>,
        config_path: impl Into<PathBuf>,
        workspace_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            sigil_binary: sigil_binary.into(),
            config_path: Some(config_path.into()),
            workspace_root: workspace_root.into(),
        }
    }

    /// Creates a launch request that lets `sigil` resolve the per-user configuration.
    ///
    /// The workspace root remains the child working directory, so a default `"."` workspace
    /// setting resolves to the folder selected by the desktop user.
    #[must_use]
    pub fn with_implicit_user_config(
        sigil_binary: impl Into<PathBuf>,
        workspace_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            sigil_binary: sigil_binary.into(),
            config_path: None,
            workspace_root: workspace_root.into(),
        }
    }

    fn validate(&self) -> Result<(), DesktopLaunchError> {
        if !self.sigil_binary.is_file() {
            return Err(DesktopLaunchError::InvalidRequest(
                "sigil binary is not a file",
            ));
        }
        if self
            .config_path
            .as_ref()
            .is_some_and(|config_path| !config_path.is_file())
        {
            return Err(DesktopLaunchError::InvalidRequest(
                "configuration is not a file",
            ));
        }
        if !self.workspace_root.is_dir() {
            return Err(DesktopLaunchError::InvalidRequest(
                "workspace root is not a directory",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for DesktopLaunchRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopLaunchRequest")
            .field("sigil_binary", &"<local path>")
            .field("config_path", &"<local path>")
            .field("workspace_root", &"<local path>")
            .finish()
    }
}

/// Typed, path-free launcher failures safe to project into a native-shell status.
#[derive(Debug, Error)]
pub enum DesktopLaunchError {
    /// Local input is missing or has the wrong filesystem type.
    #[error("desktop launch request is invalid: {0}")]
    InvalidRequest(&'static str),
    /// The HTTP client could not be constructed.
    #[error("desktop bootstrap HTTP client is unavailable")]
    HttpClientUnavailable,
    /// Random token generation failed.
    #[error("desktop bearer generation failed")]
    BearerGenerationFailed,
    /// The child process could not be started.
    #[error("desktop server process could not be spawned")]
    Spawn(#[source] io::Error),
    /// A required stdio ownership pipe was unavailable.
    #[error("desktop server process did not expose required stdio pipes")]
    MissingPipe,
    /// The platform could not establish process-tree ownership.
    #[error("desktop server process-tree ownership is unavailable")]
    ProcessOwnershipUnavailable,
    /// A spawned process tree could not be proven quiescent after launch failed.
    #[error("desktop server cleanup could not be completed")]
    CleanupFailed,
    /// Readiness was not established before the configured deadline.
    #[error("desktop server readiness timed out")]
    ReadinessTimedOut,
    /// The child closed stdout before publishing readiness.
    #[error("desktop server exited before publishing readiness")]
    ReadinessClosed,
    /// The single readiness record exceeded its hard cap.
    #[error("desktop server readiness record exceeded its size limit")]
    ReadinessTooLarge,
    /// Startup stdout was not a valid exact metadata object.
    #[error("desktop server readiness record is invalid")]
    InvalidReadinessRecord,
    /// The child metadata is valid JSON but incompatible with this client.
    #[error("desktop server is incompatible: {0}")]
    IncompatibleServer(&'static str),
    /// The authenticated metadata endpoint could not be reached.
    #[error("desktop server metadata request failed")]
    MetadataRequestFailed,
    /// The metadata endpoint rejected the private bearer or route.
    #[error("desktop server metadata request returned HTTP {status}")]
    MetadataRejected {
        /// HTTP status returned by the loopback child.
        status: u16,
    },
    /// The metadata response exceeded its hard cap.
    #[error("desktop server metadata response exceeded its size limit")]
    MetadataTooLarge,
    /// The authenticated endpoint did not return valid exact metadata.
    #[error("desktop server metadata response is invalid")]
    InvalidMetadataResponse,
    /// Stdout bootstrap and authenticated metadata disagreed.
    #[error("desktop server readiness and authenticated metadata do not match")]
    MetadataMismatch,
}

/// Configures and launches one server process at a time.
#[derive(Debug, Clone, Copy)]
pub struct DesktopLauncher {
    startup_timeout: Duration,
    shutdown_timeout: Duration,
}

impl DesktopLauncher {
    /// Creates a launcher with explicit bounded readiness and graceful shutdown deadlines.
    #[must_use]
    pub fn with_timeouts(startup_timeout: Duration, shutdown_timeout: Duration) -> Self {
        Self {
            startup_timeout,
            shutdown_timeout,
        }
    }

    /// Launches and authenticates one workspace-owned `sigil serve` child.
    ///
    /// # Errors
    ///
    /// Fails closed when local inputs, process ownership, bounded startup parsing, loopback
    /// metadata, authentication, or compatibility validation fail. Any spawned child is cleaned
    /// up before the error is returned.
    pub async fn launch(
        &self,
        request: DesktopLaunchRequest,
    ) -> Result<DesktopServerProcess, DesktopLaunchError> {
        request.validate()?;
        let client = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| DesktopLaunchError::HttpClientUnavailable)?;
        let bearer = Arc::new(DesktopBearerToken::generate()?);

        let mut command = build_server_command(&request, bearer.expose());
        sigil_process::configure_process_tree(command.as_std_mut());

        let mut child = command.spawn().map_err(DesktopLaunchError::Spawn)?;
        let process_id = match child.id() {
            Some(process_id) => process_id,
            None => {
                return Err(cleanup_unowned_error(
                    &mut child,
                    DesktopLaunchError::ProcessOwnershipUnavailable,
                )
                .await);
            }
        };
        let process_owner = match sigil_process::ProcessTreeOwnerGuard::assign(Some(process_id)) {
            Ok(owner) => owner,
            Err(_) => {
                return Err(cleanup_unowned_error(
                    &mut child,
                    DesktopLaunchError::ProcessOwnershipUnavailable,
                )
                .await);
            }
        };
        let mut owner_stdin = match child.stdin.take() {
            Some(stdin) => Some(stdin),
            None => {
                return Err(cleanup_owned_error(
                    &mut child,
                    process_id,
                    None,
                    DesktopLaunchError::MissingPipe,
                )
                .await);
            }
        };
        let mut stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                return Err(cleanup_owned_error(
                    &mut child,
                    process_id,
                    owner_stdin.take(),
                    DesktopLaunchError::MissingPipe,
                )
                .await);
            }
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                return Err(cleanup_owned_error(
                    &mut child,
                    process_id,
                    owner_stdin.take(),
                    DesktopLaunchError::MissingPipe,
                )
                .await);
            }
        };
        let stderr_task = tokio::spawn(drain_pipe(stderr));
        let deadline = Instant::now() + self.startup_timeout;

        let startup_line = match timeout_at(deadline, read_startup_line(&mut stdout)).await {
            Ok(Ok(line)) => line,
            Ok(Err(error)) => {
                stderr_task.abort();
                return Err(
                    cleanup_owned_error(&mut child, process_id, owner_stdin.take(), error).await,
                );
            }
            Err(_) => {
                stderr_task.abort();
                return Err(cleanup_owned_error(
                    &mut child,
                    process_id,
                    owner_stdin.take(),
                    DesktopLaunchError::ReadinessTimedOut,
                )
                .await);
            }
        };
        let server_info = match serde_json::from_slice::<DesktopServerInfo>(&startup_line) {
            Ok(server_info) => server_info,
            Err(_) => {
                stderr_task.abort();
                return Err(cleanup_owned_error(
                    &mut child,
                    process_id,
                    owner_stdin.take(),
                    DesktopLaunchError::InvalidReadinessRecord,
                )
                .await);
            }
        };
        let address = match server_info
            .validate()
            .map_err(DesktopLaunchError::IncompatibleServer)
        {
            Ok(address) => address,
            Err(error) => {
                stderr_task.abort();
                return Err(
                    cleanup_owned_error(&mut child, process_id, owner_stdin.take(), error).await,
                );
            }
        };
        let metadata = match timeout_at(
            deadline,
            fetch_server_info(&client, address, bearer.expose()),
        )
        .await
        {
            Ok(Ok(metadata)) => metadata,
            Ok(Err(error)) => {
                stderr_task.abort();
                return Err(
                    cleanup_owned_error(&mut child, process_id, owner_stdin.take(), error).await,
                );
            }
            Err(_) => {
                stderr_task.abort();
                return Err(cleanup_owned_error(
                    &mut child,
                    process_id,
                    owner_stdin.take(),
                    DesktopLaunchError::ReadinessTimedOut,
                )
                .await);
            }
        };
        if metadata != server_info {
            stderr_task.abort();
            return Err(cleanup_owned_error(
                &mut child,
                process_id,
                owner_stdin.take(),
                DesktopLaunchError::MetadataMismatch,
            )
            .await);
        }
        match child.try_wait() {
            Ok(None) => {}
            Ok(Some(_)) | Err(_) => {
                stderr_task.abort();
                return Err(cleanup_owned_error(
                    &mut child,
                    process_id,
                    owner_stdin.take(),
                    DesktopLaunchError::ReadinessClosed,
                )
                .await);
            }
        }
        let stdout_task = tokio::spawn(drain_pipe(stdout));

        let desktop_client = DesktopHttpClient::new(client, address, Arc::clone(&bearer));
        Ok(DesktopServerProcess {
            child: Some(child),
            owner_stdin,
            process_id,
            process_owner,
            client: desktop_client,
            server_info,
            address,
            shutdown_timeout: self.shutdown_timeout,
            stdout_task: Some(stdout_task),
            stderr_task: Some(stderr_task),
        })
    }
}

fn build_server_command(request: &DesktopLaunchRequest, bearer: &str) -> Command {
    let mut command = Command::new(&request.sigil_binary);
    command
        .current_dir(&request.workspace_root)
        .env("SIGIL_HTTP_TOKEN", bearer);
    if let Some(config_path) = &request.config_path {
        command.arg("--config").arg(config_path);
    }
    command
        .args([
            "serve",
            "--startup-output",
            "json",
            "--shutdown-on-stdin-close",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    command
}

impl Default for DesktopLauncher {
    fn default() -> Self {
        Self::with_timeouts(Duration::from_secs(15), Duration::from_secs(15))
    }
}

/// A ready, authenticated server child fully owned by the desktop backend.
pub struct DesktopServerProcess {
    child: Option<Child>,
    owner_stdin: Option<ChildStdin>,
    process_id: u32,
    process_owner: sigil_process::ProcessTreeOwnerGuard,
    client: DesktopHttpClient,
    server_info: DesktopServerInfo,
    address: SocketAddr,
    shutdown_timeout: Duration,
    stdout_task: Option<JoinHandle<()>>,
    stderr_task: Option<JoinHandle<()>>,
}

impl DesktopServerProcess {
    /// Returns the authenticated metadata accepted during launch.
    #[must_use]
    pub fn server_info(&self) -> &DesktopServerInfo {
        &self.server_info
    }

    /// Returns the validated loopback address. The bearer remains private to this crate.
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Returns a cloneable typed client whose bearer remains opaque to callers.
    #[must_use]
    pub fn client(&self) -> DesktopHttpClient {
        self.client.clone()
    }

    pub(crate) fn try_exit_status(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.as_mut().map_or_else(
            || Err(io::Error::other("desktop server child is unavailable")),
            Child::try_wait,
        )
    }

    /// Closes the owner channel and waits for graceful drain before forcing process-tree cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error when the child cannot be waited, the forced tree cannot be terminated, or
    /// the direct child cannot be reaped within the fallback deadline.
    pub async fn shutdown(mut self) -> Result<DesktopShutdownReport, DesktopShutdownError> {
        self.owner_stdin.take();
        if self.shutdown_timeout.is_zero() {
            return self.shutdown_after_deadline().await;
        }

        let wait_result = {
            let child = self
                .child
                .as_mut()
                .ok_or(DesktopShutdownError::ChildUnavailable)?;
            timeout(self.shutdown_timeout, child.wait()).await
        };
        match wait_result {
            Ok(Ok(status)) => {
                self.child.take();
                self.finish_pipe_tasks().await;
                Ok(DesktopShutdownReport::from_status(
                    DesktopShutdownKind::Graceful,
                    status,
                ))
            }
            Ok(Err(_)) => Err(DesktopShutdownError::WaitFailed),
            Err(_) => self.shutdown_after_deadline().await,
        }
    }

    async fn shutdown_after_deadline(
        &mut self,
    ) -> Result<DesktopShutdownReport, DesktopShutdownError> {
        let tree_terminated = self.process_owner.terminate().is_ok();
        let child = self
            .child
            .as_mut()
            .ok_or(DesktopShutdownError::ChildUnavailable)?;
        let status = wait_after_fallback(child).await?;
        let kind = if status.success() {
            DesktopShutdownKind::GracefulAfterDeadline
        } else if tree_terminated {
            DesktopShutdownKind::Forced
        } else {
            return Err(DesktopShutdownError::TerminationFailed);
        };
        self.child.take();
        self.finish_pipe_tasks().await;
        Ok(DesktopShutdownReport::from_status(kind, status))
    }

    async fn finish_pipe_tasks(&mut self) {
        finish_pipe_task(&mut self.stdout_task).await;
        finish_pipe_task(&mut self.stderr_task).await;
    }
}

impl fmt::Debug for DesktopServerProcess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopServerProcess")
            .field("process_id", &self.process_id)
            .field("server_info", &self.server_info)
            .field("address", &self.address)
            .field("bearer", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl Drop for DesktopServerProcess {
    fn drop(&mut self) {
        self.owner_stdin.take();
        if let Some(child) = self.child.as_mut() {
            let should_terminate = !matches!(child.try_wait(), Ok(Some(_)));
            if should_terminate {
                let _ = self.process_owner.terminate();
                let _ = child.start_kill();
            }
        }
        if let Some(task) = self.stdout_task.take() {
            task.abort();
        }
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
    }
}

/// Whether the server completed owner-channel drain or required fallback termination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopShutdownKind {
    /// Owner-pipe closure completed before the configured deadline.
    Graceful,
    /// Owner-pipe closure completed successfully after the grace deadline raced with fallback.
    GracefulAfterDeadline,
    /// The deadline elapsed and process-tree fallback cleanup was invoked.
    Forced,
}

/// Observable, secret-free result of stopping one desktop server child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopShutdownReport {
    /// Shutdown path used by the supervisor.
    pub kind: DesktopShutdownKind,
    /// Native exit code when the platform reports one.
    pub exit_code: Option<i32>,
    /// Whether the native status represented successful termination.
    pub success: bool,
}

impl DesktopShutdownReport {
    fn from_status(kind: DesktopShutdownKind, status: ExitStatus) -> Self {
        Self {
            kind,
            exit_code: status.code(),
            success: status.success(),
        }
    }
}

/// Typed failures from the explicit shutdown path.
#[derive(Debug, Error)]
pub enum DesktopShutdownError {
    /// The process handle was already consumed.
    #[error("desktop server child is unavailable")]
    ChildUnavailable,
    /// Waiting for the direct child failed.
    #[error("desktop server child wait failed")]
    WaitFailed,
    /// Process-tree termination failed and the server did not prove a successful late drain.
    #[error("desktop server process tree could not be terminated")]
    TerminationFailed,
    /// The direct child did not become reapable after forced termination.
    #[error("desktop server child could not be reaped before the fallback deadline")]
    ReapTimedOut,
}

async fn read_startup_line<R>(reader: &mut R) -> Result<Vec<u8>, DesktopLaunchError>
where
    R: AsyncRead + Unpin,
{
    let mut line = Vec::with_capacity(512);
    let mut byte = [0_u8; 1];
    loop {
        let read = reader
            .read(&mut byte)
            .await
            .map_err(|_| DesktopLaunchError::ReadinessClosed)?;
        if read == 0 {
            return Err(DesktopLaunchError::ReadinessClosed);
        }
        if byte[0] == b'\n' {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                return Err(DesktopLaunchError::InvalidReadinessRecord);
            }
            return Ok(line);
        }
        if line.len() >= MAX_BOOTSTRAP_BYTES {
            return Err(DesktopLaunchError::ReadinessTooLarge);
        }
        line.push(byte[0]);
    }
}

async fn fetch_server_info(
    client: &Client,
    address: SocketAddr,
    bearer: &str,
) -> Result<DesktopServerInfo, DesktopLaunchError> {
    let url = format!("http://{address}/server-info");
    let mut response = client
        .get(url)
        .bearer_auth(bearer)
        .send()
        .await
        .map_err(|_| DesktopLaunchError::MetadataRequestFailed)?;
    let status = response.status();
    if status.as_u16() != 200 {
        return Err(DesktopLaunchError::MetadataRejected {
            status: status.as_u16(),
        });
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_SERVER_INFO_BYTES as u64)
    {
        return Err(DesktopLaunchError::MetadataTooLarge);
    }
    let mut body = Vec::with_capacity(512);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| DesktopLaunchError::MetadataRequestFailed)?
    {
        if body.len().saturating_add(chunk.len()) > MAX_SERVER_INFO_BYTES {
            return Err(DesktopLaunchError::MetadataTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    let server_info = serde_json::from_slice::<DesktopServerInfo>(&body)
        .map_err(|_| DesktopLaunchError::InvalidMetadataResponse)?;
    server_info
        .validate()
        .map_err(DesktopLaunchError::IncompatibleServer)?;
    Ok(server_info)
}

async fn drain_pipe<R>(mut reader: R)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0_u8; 4096];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

async fn cleanup_unowned_child(child: &mut Child) -> Result<(), DesktopLaunchError> {
    let _ = child.start_kill();
    let _ = timeout(FORCED_REAP_TIMEOUT, child.wait()).await;
    Err(DesktopLaunchError::CleanupFailed)
}

async fn cleanup_owned_child(
    child: &mut Child,
    process_id: u32,
    owner_stdin: Option<ChildStdin>,
) -> Result<(), DesktopLaunchError> {
    drop(owner_stdin);
    if let Ok(Ok(_)) = timeout(FAILED_LAUNCH_GRACE, child.wait()).await {
        return Ok(());
    }
    let tree_terminated = sigil_process::terminate_owned_process_tree(process_id).is_ok();
    let _ = child.start_kill();
    match timeout(FORCED_REAP_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) if tree_terminated || status.success() => Ok(()),
        Ok(Ok(_) | Err(_)) | Err(_) => Err(DesktopLaunchError::CleanupFailed),
    }
}

async fn wait_after_fallback(child: &mut Child) -> Result<ExitStatus, DesktopShutdownError> {
    match timeout(FORCED_REAP_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(_)) => Err(DesktopShutdownError::WaitFailed),
        Err(_) => {
            let _ = child.start_kill();
            timeout(FORCED_REAP_TIMEOUT, child.wait())
                .await
                .map_err(|_| DesktopShutdownError::ReapTimedOut)?
                .map_err(|_| DesktopShutdownError::WaitFailed)
        }
    }
}

async fn cleanup_unowned_error(
    child: &mut Child,
    original: DesktopLaunchError,
) -> DesktopLaunchError {
    cleanup_unowned_child(child).await.err().unwrap_or(original)
}

async fn cleanup_owned_error(
    child: &mut Child,
    process_id: u32,
    owner_stdin: Option<ChildStdin>,
    original: DesktopLaunchError,
) -> DesktopLaunchError {
    cleanup_owned_child(child, process_id, owner_stdin)
        .await
        .err()
        .unwrap_or(original)
}

async fn finish_pipe_task(task: &mut Option<JoinHandle<()>>) {
    let Some(mut task) = task.take() else {
        return;
    };
    if timeout(PIPE_TASK_FINISH_TIMEOUT, &mut task).await.is_err() {
        task.abort();
        let _ = task.await;
    }
}

#[cfg(test)]
#[path = "tests/launcher_tests.rs"]
mod tests;
