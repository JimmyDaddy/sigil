use super::*;
use std::collections::VecDeque;

const MCP_STDERR_HEAD_LIMIT_BYTES: usize = 16 * 1024;
const MCP_STDERR_TAIL_LIMIT_BYTES: usize = 48 * 1024;
const MCP_STDERR_HARD_LIMIT_BYTES: u64 = 8 * 1024 * 1024;
const MCP_PROCESS_EXIT_GRACE: Duration = Duration::from_millis(500);
const MCP_CLEANUP_COMMAND_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpProcessClass {
    LocalStdioConfigured,
    LocalStdioPluginDeclared,
    LocalStdioSandboxed,
    RemoteOrExternal,
    UnsupportedLongLivedBackend,
}

impl McpProcessClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalStdioConfigured => "local_stdio_configured",
            Self::LocalStdioPluginDeclared => "local_stdio_plugin_declared",
            Self::LocalStdioSandboxed => "local_stdio_sandboxed",
            Self::RemoteOrExternal => "remote_or_external",
            Self::UnsupportedLongLivedBackend => "unsupported_long_lived_backend",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpProcessCoverage {
    LocalStdioOutsideSandbox,
    LocalStdioSandboxed,
    RemoteOrExternal,
    Unsupported,
}

impl McpProcessCoverage {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalStdioOutsideSandbox => "local_stdio_outside_sandbox",
            Self::LocalStdioSandboxed => "local_stdio_sandboxed",
            Self::RemoteOrExternal => "remote_or_external",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProcessLaunchRequest {
    pub server_name: String,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub environment: ResolvedProcessEnvironment,
    pub launch_static_fingerprint: String,
    pub startup_timeout_secs: u64,
    pub classification: McpProcessClass,
}

impl McpProcessLaunchRequest {
    pub(super) fn from_config(
        config: &McpServerConfig,
        working_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let environment = resolve_extension_process_environment(&config.inherit_env)?;
        let fingerprint_working_dir = working_dir
            .clone()
            .map(Ok)
            .unwrap_or_else(std::env::current_dir)
            .context("failed to resolve MCP launch cwd")?;
        let static_binding = super::tools::mcp_launch_static_binding(
            config,
            &fingerprint_working_dir,
            &environment,
        )?;
        Ok(Self {
            server_name: config.name.clone(),
            command: static_binding.executable.to_string_lossy().into_owned(),
            args: config.args.clone(),
            working_dir: static_binding.working_dir.or(working_dir),
            environment,
            launch_static_fingerprint: static_binding.fingerprint,
            startup_timeout_secs: config.startup_timeout_secs,
            classification: McpProcessClass::LocalStdioConfigured,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct McpProcessLaunchReceipt {
    pub server_name: String,
    pub classification: McpProcessClass,
    pub coverage: McpProcessCoverage,
    pub backend: Option<ExecutionBackendKind>,
    pub backend_capabilities: Option<ExecutionBackendCapabilities>,
    pub sandbox_profile: Option<ExecutionSandboxProfile>,
    #[serde(default)]
    pub network: ExecutionNetworkReceipt,
    #[serde(default)]
    pub environment_policy: ProcessEnvironmentPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment_baseline_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment_grant_names: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub environment_static_fingerprint: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub environment_live_fingerprint: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub launch_static_fingerprint: String,
}

impl McpProcessLaunchReceipt {
    #[must_use]
    pub fn local_outside_sandbox(request: &McpProcessLaunchRequest) -> Self {
        Self {
            server_name: request.server_name.clone(),
            classification: request.classification,
            coverage: McpProcessCoverage::LocalStdioOutsideSandbox,
            backend: Some(ExecutionBackendKind::Local),
            backend_capabilities: Some(ExecutionBackendCapabilities::default()),
            sandbox_profile: Some(ExecutionSandboxProfile::Unconfined),
            network: ExecutionNetworkReceipt::unknown(
                "local MCP launcher does not report network enforcement",
            ),
            environment_policy: request.environment.policy(),
            environment_baseline_names: request.environment.baseline_names().to_vec(),
            environment_grant_names: request.environment.grant_names().to_vec(),
            environment_static_fingerprint: request.environment.static_fingerprint().to_owned(),
            environment_live_fingerprint: request.environment.live_fingerprint().to_owned(),
            launch_static_fingerprint: request.launch_static_fingerprint.clone(),
        }
    }

    #[must_use]
    pub fn audit_metadata(&self) -> BTreeMap<String, String> {
        let mut metadata = BTreeMap::from([
            (
                "mcp_process_class".to_owned(),
                self.classification.as_str().to_owned(),
            ),
            (
                "mcp_process_coverage".to_owned(),
                self.coverage.as_str().to_owned(),
            ),
        ]);
        if let Some(backend) = self.backend {
            metadata.insert(
                "mcp_process_backend".to_owned(),
                backend.as_str().to_owned(),
            );
        }
        if let Some(profile) = self.sandbox_profile {
            metadata.insert(
                "mcp_process_profile".to_owned(),
                mcp_sandbox_profile_label(profile).to_owned(),
            );
        }
        if let Some(capabilities) = self.backend_capabilities {
            metadata.insert(
                "mcp_process_backend_capabilities".to_owned(),
                mcp_backend_capability_labels(capabilities).join(","),
            );
        }
        metadata.insert(
            "mcp_process_network".to_owned(),
            self.network.policy.as_str().to_owned(),
        );
        metadata.insert(
            "mcp_environment_policy".to_owned(),
            self.environment_policy.as_str().to_owned(),
        );
        metadata.insert(
            "mcp_environment_baseline_names".to_owned(),
            self.environment_baseline_names.join(","),
        );
        metadata.insert(
            "mcp_environment_grant_names".to_owned(),
            self.environment_grant_names.join(","),
        );
        metadata.insert(
            "mcp_environment_grant_source".to_owned(),
            "parent_environment".to_owned(),
        );
        metadata.insert(
            "mcp_environment_static_fingerprint".to_owned(),
            self.environment_static_fingerprint.clone(),
        );
        metadata.insert(
            "mcp_environment_live_fingerprint".to_owned(),
            self.environment_live_fingerprint.clone(),
        );
        metadata.insert(
            "mcp_launch_static_fingerprint".to_owned(),
            self.launch_static_fingerprint.clone(),
        );
        metadata
    }
}

pub(super) fn mcp_sandbox_profile_label(profile: ExecutionSandboxProfile) -> &'static str {
    match profile {
        ExecutionSandboxProfile::Unconfined => "unconfined",
        ExecutionSandboxProfile::WorkspaceWrite => "workspace_write",
        ExecutionSandboxProfile::BuildOffline => "build_offline",
        ExecutionSandboxProfile::BuildNetworked => "build_networked",
    }
}

pub(super) fn mcp_backend_capability_labels(
    capabilities: ExecutionBackendCapabilities,
) -> Vec<&'static str> {
    let mut labels = Vec::new();
    if capabilities.filesystem_isolation {
        labels.push("filesystem");
    }
    if capabilities.network_isolation {
        labels.push("network");
    }
    if capabilities.process_isolation {
        labels.push("process");
    }
    if capabilities.resource_limits {
        labels.push("resource");
    }
    if capabilities.persistent_pty {
        labels.push("persistent_pty");
    }
    if capabilities.workspace_snapshot {
        labels.push("workspace_snapshot");
    }
    if labels.is_empty() {
        labels.push("none");
    }
    labels
}

pub struct McpProcessLaunch {
    pub child: Child,
    pub receipt: McpProcessLaunchReceipt,
}

pub trait McpProcessLauncher: Send + Sync {
    /// Launches one local MCP stdio process and returns its coverage receipt.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured process cannot be spawned or a required sandbox
    /// coverage cannot be provided.
    fn launch(&self, request: McpProcessLaunchRequest) -> Result<McpProcessLaunch>;
}

#[derive(Debug)]
pub struct LocalMcpProcessLauncher;

impl McpProcessLauncher for LocalMcpProcessLauncher {
    fn launch(&self, request: McpProcessLaunchRequest) -> Result<McpProcessLaunch> {
        let mut command = Command::new(&request.command);
        command
            .args(&request.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        configure_mcp_process_group(&mut command);
        if let Some(working_dir) = &request.working_dir {
            command.current_dir(working_dir);
        }
        command.env_clear();
        for (name, value) in request.environment.variables() {
            command.env(name, value.expose_secret());
        }
        let child = command
            .spawn()
            .with_context(|| format!("failed to spawn MCP server {}", request.server_name))?;
        Ok(McpProcessLaunch {
            child,
            receipt: McpProcessLaunchReceipt::local_outside_sandbox(&request),
        })
    }
}

#[derive(Debug, Default)]
pub(super) struct McpStderrSummary {
    pub(super) total_bytes: u64,
    pub(super) truncated: bool,
    pub(super) hard_limit_exceeded: bool,
    pub(super) head: Vec<u8>,
    pub(super) tail: Vec<u8>,
}

#[derive(Debug, Clone)]
pub(super) enum McpStderrFault {
    HardLimit { total_bytes: u64 },
    ReaderFailed { total_bytes: u64, reason: String },
}

impl McpStderrFault {
    pub(super) fn reason(&self) -> String {
        match self {
            Self::HardLimit { total_bytes } => {
                format!("MCP server stderr exceeded hard limit after at least {total_bytes} bytes")
            }
            Self::ReaderFailed {
                total_bytes,
                reason,
            } => format!("MCP server stderr reader failed after {total_bytes} bytes: {reason}"),
        }
    }

    pub(super) fn terminal_cause(&self) -> McpTerminalCause {
        match self {
            Self::HardLimit { total_bytes } => McpTerminalCause::StderrLimit {
                total_bytes: *total_bytes,
                limit_bytes: MCP_STDERR_HARD_LIMIT_BYTES,
            },
            Self::ReaderFailed {
                total_bytes,
                reason,
            } => McpTerminalCause::StderrReaderFailed {
                total_bytes: *total_bytes,
                reason: reason.clone(),
            },
        }
    }
}

pub(super) async fn drain_mcp_stderr(
    stderr: ChildStderr,
    hard_limit_sender: tokio::sync::oneshot::Sender<McpStderrFault>,
    faulted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    fault_record: std::sync::Arc<std::sync::Mutex<Option<McpStderrFault>>>,
) -> McpStderrSummary {
    drain_mcp_stderr_reader(stderr, hard_limit_sender, faulted, fault_record).await
}

pub(super) async fn drain_mcp_stderr_reader<R>(
    mut stderr: R,
    hard_limit_sender: tokio::sync::oneshot::Sender<McpStderrFault>,
    faulted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    fault_record: std::sync::Arc<std::sync::Mutex<Option<McpStderrFault>>>,
) -> McpStderrSummary
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = [0_u8; 4096];
    let mut summary = McpStderrSummary::default();
    let mut tail = VecDeque::with_capacity(MCP_STDERR_TAIL_LIMIT_BYTES);
    let mut hard_limit_sender = Some(hard_limit_sender);
    loop {
        let read = match stderr.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                if let Some(sender) = hard_limit_sender.take() {
                    let reason = error.to_string().chars().take(512).collect();
                    let fault = McpStderrFault::ReaderFailed {
                        total_bytes: summary.total_bytes,
                        reason,
                    };
                    *fault_record
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fault.clone());
                    faulted.store(true, std::sync::atomic::Ordering::Release);
                    let _ = sender.send(fault);
                }
                break;
            }
        };
        summary.total_bytes = summary
            .total_bytes
            .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        let bytes = &buffer[..read];
        let remaining_head = MCP_STDERR_HEAD_LIMIT_BYTES.saturating_sub(summary.head.len());
        let head_bytes = remaining_head.min(bytes.len());
        summary.head.extend_from_slice(&bytes[..head_bytes]);
        for byte in &bytes[head_bytes..] {
            if tail.len() == MCP_STDERR_TAIL_LIMIT_BYTES {
                tail.pop_front();
                summary.truncated = true;
            }
            tail.push_back(*byte);
        }
        summary.hard_limit_exceeded = summary.total_bytes > MCP_STDERR_HARD_LIMIT_BYTES;
        if summary.hard_limit_exceeded
            && let Some(sender) = hard_limit_sender.take()
        {
            let fault = McpStderrFault::HardLimit {
                total_bytes: summary.total_bytes,
            };
            *fault_record
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fault.clone());
            faulted.store(true, std::sync::atomic::Ordering::Release);
            let _ = sender.send(fault);
        }
    }
    summary.tail = tail.into_iter().collect();
    summary.truncated |= summary.total_bytes
        > u64::try_from(MCP_STDERR_HEAD_LIMIT_BYTES + MCP_STDERR_TAIL_LIMIT_BYTES)
            .unwrap_or(u64::MAX);
    summary
}

#[derive(Debug, Clone)]
pub(super) struct McpProcessCleanupSummary {
    pub(super) completed: bool,
    pub(super) reason: String,
}

impl McpProcessCleanupSummary {
    fn completed(reason: impl Into<String>) -> Self {
        Self {
            completed: true,
            reason: reason.into(),
        }
    }

    fn failed(reason: impl Into<String>) -> Self {
        Self {
            completed: false,
            reason: reason.into(),
        }
    }
}

pub(super) async fn terminate_mcp_process(child: &mut Child) -> McpProcessCleanupSummary {
    let process_id = child.id();
    match child.try_wait() {
        Ok(Some(_)) => {
            return cleanup_after_mcp_leader_reaped(process_id, "process already exited").await;
        }
        Ok(None) => {}
        Err(error) => {
            return McpProcessCleanupSummary::failed(format!(
                "failed to inspect MCP process before cleanup: {error}"
            ));
        }
    }

    #[cfg(not(windows))]
    if let Ok(status) = tokio::time::timeout(MCP_PROCESS_EXIT_GRACE, child.wait()).await {
        return match status {
            Ok(_) => {
                cleanup_after_mcp_leader_reaped(process_id, "process exited after stdin closed")
                    .await
            }
            Err(error) => McpProcessCleanupSummary::failed(format!(
                "failed to wait for MCP process after stdin closed: {error}"
            )),
        };
    }

    #[cfg(unix)]
    if let Some(process_id) = process_id {
        if let Err(error) = signal_mcp_process_group(process_id, "TERM").await {
            let fallback = kill_and_reap_child(child).await;
            return McpProcessCleanupSummary::failed(format!(
                "failed to terminate MCP process group {process_id}: {error}; {}",
                fallback.reason
            ));
        }
        if let Ok(status) = tokio::time::timeout(MCP_PROCESS_EXIT_GRACE, child.wait()).await {
            return match status {
                Ok(_) => {
                    cleanup_after_mcp_leader_reaped(
                        Some(process_id),
                        format!("terminated and reaped MCP process-group leader {process_id}"),
                    )
                    .await
                }
                Err(error) => McpProcessCleanupSummary::failed(format!(
                    "failed to reap terminated MCP process group {process_id}: {error}"
                )),
            };
        }
        if let Err(error) = signal_mcp_process_group(process_id, "KILL").await {
            let fallback = kill_and_reap_child(child).await;
            return McpProcessCleanupSummary::failed(format!(
                "failed to kill MCP process group {process_id}: {error}; {}",
                fallback.reason
            ));
        }
        return match tokio::time::timeout(MCP_PROCESS_EXIT_GRACE, child.wait()).await {
            Ok(Ok(_)) => {
                cleanup_after_mcp_leader_reaped(
                    Some(process_id),
                    format!("killed and reaped MCP process-group leader {process_id}"),
                )
                .await
            }
            Ok(Err(error)) => McpProcessCleanupSummary::failed(format!(
                "killed MCP process group {process_id} but failed to reap it: {error}"
            )),
            Err(_) => McpProcessCleanupSummary::failed(format!(
                "killed MCP process group {process_id} but child reap exceeded the bounded grace"
            )),
        };
    }

    #[cfg(windows)]
    if let Some(process_id) = process_id {
        let process_id_text = process_id.to_string();
        let mut command = Command::new("taskkill");
        command.args(["/PID", process_id_text.as_str(), "/T", "/F"]);
        let taskkill = bounded_cleanup_command_status(
            command,
            format!("taskkill for MCP process tree {process_id}"),
        )
        .await;
        match taskkill {
            Ok(status) if status.success() => {
                return match tokio::time::timeout(MCP_PROCESS_EXIT_GRACE, child.wait()).await {
                    Ok(Ok(_)) => McpProcessCleanupSummary::completed(format!(
                        "taskkill terminated the MCP process tree {process_id} and the child was reaped"
                    )),
                    Ok(Err(error)) => McpProcessCleanupSummary::failed(format!(
                        "taskkill terminated MCP process tree {process_id} but the child was not reaped: {error}"
                    )),
                    Err(_) => McpProcessCleanupSummary::failed(format!(
                        "taskkill terminated MCP process tree {process_id} but child reap exceeded the bounded grace"
                    )),
                };
            }
            Ok(status) => {
                let fallback = kill_and_reap_child(child).await;
                return McpProcessCleanupSummary::failed(format!(
                    "taskkill for MCP process tree {process_id} exited with {status}; direct-child fallback: {}",
                    fallback.reason
                ));
            }
            Err(error) => {
                let fallback = kill_and_reap_child(child).await;
                return McpProcessCleanupSummary::failed(format!(
                    "failed to invoke taskkill for MCP process tree {process_id}: {error}; direct-child fallback: {}",
                    fallback.reason
                ));
            }
        }
    }

    kill_and_reap_child(child).await
}

async fn cleanup_after_mcp_leader_reaped(
    process_id: Option<u32>,
    reason: impl Into<String>,
) -> McpProcessCleanupSummary {
    let reason = reason.into();
    #[cfg(unix)]
    if let Some(process_id) = process_id {
        if let Err(signal_error) = signal_mcp_process_group(process_id, "TERM").await {
            return match process_group_is_alive(process_id).await {
                Ok(false) => McpProcessCleanupSummary::completed(format!(
                    "{reason}; no remaining process-group descendants"
                )),
                Ok(true) => McpProcessCleanupSummary::failed(format!(
                    "{reason}; process group {process_id} is still alive after TERM failed: {signal_error}"
                )),
                Err(check_error) => McpProcessCleanupSummary::failed(format!(
                    "{reason}; TERM failed for process group {process_id}: {signal_error}; liveness check failed: {check_error}"
                )),
            };
        }
        tokio::time::sleep(MCP_PROCESS_EXIT_GRACE).await;
        match process_group_is_alive(process_id).await {
            Ok(false) => {
                return McpProcessCleanupSummary::completed(format!(
                    "{reason}; process-group descendants exited during grace"
                ));
            }
            Ok(true) => {}
            Err(error) => {
                return McpProcessCleanupSummary::failed(format!(
                    "{reason}; failed to verify process group {process_id} after TERM: {error}"
                ));
            }
        }
        if let Err(error) = signal_mcp_process_group(process_id, "KILL").await {
            return McpProcessCleanupSummary::failed(format!(
                "{reason}; failed to kill remaining process-group descendants: {error}"
            ));
        }
        for _ in 0..20 {
            match process_group_is_alive(process_id).await {
                Ok(false) => {
                    return McpProcessCleanupSummary::completed(format!(
                        "{reason}; killed remaining process-group descendants"
                    ));
                }
                Ok(true) => tokio::time::sleep(Duration::from_millis(25)).await,
                Err(error) => {
                    return McpProcessCleanupSummary::failed(format!(
                        "{reason}; failed to verify killed process group {process_id}: {error}"
                    ));
                }
            }
        }
        return McpProcessCleanupSummary::failed(format!(
            "{reason}; process group {process_id} remained alive after SIGKILL"
        ));
    }
    #[cfg(windows)]
    if let Some(process_id) = process_id {
        return McpProcessCleanupSummary::failed(format!(
            "{reason}; MCP process-tree cleanup is unconfirmed because leader {process_id} exited before taskkill"
        ));
    }
    #[cfg(not(any(unix, windows)))]
    if process_id.is_some() {
        return McpProcessCleanupSummary::failed(format!(
            "{reason}; MCP process-tree cleanup is unsupported on this platform"
        ));
    }
    McpProcessCleanupSummary::completed(reason)
}

async fn kill_and_reap_child(child: &mut Child) -> McpProcessCleanupSummary {
    match child.start_kill() {
        Ok(()) => match tokio::time::timeout(MCP_PROCESS_EXIT_GRACE, child.wait()).await {
            Ok(Ok(_)) => McpProcessCleanupSummary::completed("killed and reaped direct MCP child"),
            Ok(Err(error)) => McpProcessCleanupSummary::failed(format!(
                "killed direct MCP child but failed to reap it: {error}"
            )),
            Err(_) => McpProcessCleanupSummary::failed(
                "killed direct MCP child but reap exceeded the bounded grace",
            ),
        },
        Err(error) => {
            McpProcessCleanupSummary::failed(format!("failed to kill direct MCP child: {error}"))
        }
    }
}

fn configure_mcp_process_group(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
}

#[cfg(unix)]
async fn signal_mcp_process_group(process_id: u32, signal: &str) -> Result<()> {
    let label = format!("kill -{signal} -{process_id}");
    let mut command = Command::new("kill");
    command
        .arg(format!("-{signal}"))
        .arg(format!("-{process_id}"));
    let status = bounded_cleanup_command_status(command, &label).await?;
    if !status.success() {
        bail!("{label} exited with {status}");
    }
    Ok(())
}

#[cfg(unix)]
async fn process_group_is_alive(process_id: u32) -> Result<bool> {
    let mut command = Command::new("kill");
    command.arg("-0").arg(format!("-{process_id}"));
    let status = bounded_cleanup_command_status(
        command,
        format!("kill -0 process-group probe for {process_id}"),
    )
    .await?;
    Ok(status.success())
}

async fn bounded_cleanup_command_status(
    mut command: Command,
    label: impl AsRef<str>,
) -> Result<std::process::ExitStatus> {
    let label = label.as_ref();
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(MCP_CLEANUP_COMMAND_TIMEOUT, command.status())
        .await
        .with_context(|| format!("{label} exceeded the bounded cleanup command timeout"))?
        .with_context(|| format!("failed to invoke {label}"))
}

#[cfg(test)]
#[path = "tests/process_tests.rs"]
mod tests;
