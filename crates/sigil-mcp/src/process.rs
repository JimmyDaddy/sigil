use super::*;
use std::collections::VecDeque;

const MCP_STDERR_HEAD_LIMIT_BYTES: usize = 16 * 1024;
const MCP_STDERR_TAIL_LIMIT_BYTES: usize = 48 * 1024;
const MCP_STDERR_HARD_LIMIT_BYTES: u64 = 8 * 1024 * 1024;
const MCP_PROCESS_EXIT_GRACE: Duration = Duration::from_millis(500);

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

#[derive(Clone, PartialEq, Eq)]
pub struct McpProcessLaunchRequest {
    pub server_name: String,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub environment: ResolvedProcessEnvironment,
    pub launch_static_fingerprint: String,
    pub startup_timeout_secs: u64,
    pub classification: McpProcessClass,
    pub network_admission: ExtensionProcessNetworkAdmission,
    pub declaration: Option<McpDeclarationLaunchMetadata>,
}

impl std::fmt::Debug for McpProcessLaunchRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpProcessLaunchRequest")
            .field("server_name", &"[hidden]")
            .field("command", &"[hidden]")
            .field("args", &"[hidden]")
            .field("working_dir", &"[hidden]")
            .field("environment", &"[hidden]")
            .field("launch_static_fingerprint", &"[hidden]")
            .field("startup_timeout_secs", &self.startup_timeout_secs)
            .field("classification", &self.classification)
            .field("network_admission", &self.network_admission)
            .field("declaration", &self.declaration)
            .finish()
    }
}

impl McpProcessLaunchRequest {
    /// Resolves one legacy stdio config into a launch request.
    ///
    /// Declaration-aware launchers may call this only after validating their origin/attestation
    /// and execution base, then replace the command with the already resolved executable.
    pub fn from_config(config: &McpServerConfig, working_dir: Option<PathBuf>) -> Result<Self> {
        let (_, args, inherit_env) = config
            .stdio()
            .ok_or_else(|| anyhow!("remote MCP config cannot be launched as a stdio process"))?;
        let environment = resolve_extension_process_environment(inherit_env)?;
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
            args: args.to_vec(),
            working_dir: static_binding.working_dir.or(working_dir),
            environment,
            launch_static_fingerprint: static_binding.fingerprint,
            startup_timeout_secs: config.startup_timeout_secs,
            classification: McpProcessClass::LocalStdioConfigured,
            network_admission: ExtensionProcessNetworkAdmission::default(),
            declaration: None,
        })
    }

    #[must_use]
    pub fn with_network_admission(
        mut self,
        network_admission: ExtensionProcessNetworkAdmission,
    ) -> Self {
        self.network_admission = network_admission;
        self
    }
}

/// Secret-safe declaration identity carried through process authorization and lifecycle audit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct McpDeclarationLaunchMetadata {
    pub declared_name: String,
    pub effective_name: String,
    pub origin_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_id: Option<String>,
    pub execution_base_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<String>,
    pub projection_fingerprint: String,
    pub authorization_fingerprint: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declaration: Option<McpDeclarationLaunchMetadata>,
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
            declaration: request.declaration.clone(),
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
        if let Some(declaration) = &self.declaration {
            metadata.insert(
                "mcp_declared_name".to_owned(),
                declaration.declared_name.clone(),
            );
            metadata.insert(
                "mcp_effective_name".to_owned(),
                declaration.effective_name.clone(),
            );
            metadata.insert(
                "mcp_config_origin".to_owned(),
                declaration.origin_kind.clone(),
            );
            if let Some(origin_id) = &declaration.origin_id {
                metadata.insert("mcp_config_origin_id".to_owned(), origin_id.clone());
            }
            metadata.insert(
                "mcp_execution_base_kind".to_owned(),
                declaration.execution_base_kind.clone(),
            );
            if let Some(manifest_hash) = &declaration.manifest_hash {
                metadata.insert("mcp_manifest_hash".to_owned(), manifest_hash.clone());
            }
            if let Some(manifest_version) = &declaration.manifest_version {
                metadata.insert("mcp_manifest_version".to_owned(), manifest_version.clone());
            }
            if let Some(capability_digest) = &declaration.capability_digest {
                metadata.insert(
                    "mcp_capability_digest".to_owned(),
                    capability_digest.clone(),
                );
            }
            if let Some(release_digest) = &declaration.release_digest {
                metadata.insert("mcp_release_digest".to_owned(), release_digest.clone());
            }
            if let Some(trust) = &declaration.trust {
                metadata.insert("mcp_plugin_trust".to_owned(), trust.clone());
            }
            metadata.insert(
                "mcp_declaration_projection_fingerprint".to_owned(),
                declaration.projection_fingerprint.clone(),
            );
            metadata.insert(
                "mcp_declaration_authorization_fingerprint".to_owned(),
                declaration.authorization_fingerprint.clone(),
            );
        }
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
    pub process_owner: sigil_process::ProcessTreeOwnerGuard,
    pub receipt: McpProcessLaunchReceipt,
}

impl McpProcessLaunch {
    /// Binds a freshly spawned child to the platform process-tree owner before returning it to the
    /// MCP lifecycle. Assignment failure kills and synchronously reaps the direct child within a
    /// bounded grace so callers cannot accidentally continue with an unowned process.
    ///
    /// # Errors
    ///
    /// Returns an error when platform ownership cannot be established. The direct child is then
    /// terminated and given a bounded synchronous reap grace before the error is returned.
    pub fn owned(mut child: Child, receipt: McpProcessLaunchReceipt) -> Result<Self> {
        let process_owner = assign_mcp_process_owner(&mut child)?;
        Ok(Self {
            child,
            process_owner,
            receipt,
        })
    }
}

pub trait McpProcessLauncher: Send + Sync {
    /// Resolves declaration-aware launch material before process-subject and pin validation.
    ///
    /// The default preserves the legacy config-only launcher behavior.
    fn resolve_launch_request(
        &self,
        config: &McpServerConfig,
        fallback_working_dir: Option<PathBuf>,
    ) -> Result<McpProcessLaunchRequest> {
        McpProcessLaunchRequest::from_config(config, fallback_working_dir)
    }

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
        let planned_network = ExecutionNetworkReceipt::unknown(
            "local MCP launcher does not report network enforcement",
        );
        validate_extension_process_network_admission(
            ExecutionSandboxProfile::Unconfined,
            Some(NetworkEffect::Unknown),
            request.network_admission,
            ExecutionBackendCapabilities::default(),
            &planned_network,
            format!("mcp_server:{}", request.server_name),
        )?;
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
        McpProcessLaunch::owned(
            child,
            McpProcessLaunchReceipt::local_outside_sandbox(&request),
        )
        .with_context(|| {
            format!(
                "failed to establish process-tree ownership for MCP server {}",
                request.server_name
            )
        })
    }
}

fn assign_mcp_process_owner(child: &mut Child) -> Result<sigil_process::ProcessTreeOwnerGuard> {
    let process_id = child.id();
    match sigil_process::ProcessTreeOwnerGuard::assign(process_id) {
        Ok(owner) => Ok(owner),
        Err(error) => {
            let direct_kill = child.start_kill();
            let deadline = std::time::Instant::now() + MCP_PROCESS_EXIT_GRACE;
            let direct_wait = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break format!("reaped with {status}"),
                    Ok(None) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(None) => break "reap exceeded bounded grace".to_owned(),
                    Err(wait_error) => break format!("reap failed: {wait_error}"),
                }
            };
            bail!(
                "process-tree assignment failed: {error}; direct_kill={direct_kill:?}; direct_wait={direct_wait}"
            );
        }
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
        if let Err(error) = crate::process_group::signal_process_group(process_id, "TERM") {
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
        if let Err(error) = crate::process_group::signal_process_group(process_id, "KILL") {
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
        let terminate = sigil_process::terminate_owned_process_tree(process_id);
        if terminate.is_err() {
            let fallback = kill_and_reap_child(child).await;
            return McpProcessCleanupSummary::failed(format!(
                "failed to terminate Windows MCP Job Object {process_id}: {terminate:?}; direct-child fallback: {}",
                fallback.reason
            ));
        }
        return match tokio::time::timeout(MCP_PROCESS_EXIT_GRACE, child.wait()).await {
            Ok(Ok(_)) => McpProcessCleanupSummary::completed(format!(
                "Windows Job Object terminated MCP process tree {process_id} and the child was reaped"
            )),
            Ok(Err(error)) => McpProcessCleanupSummary::failed(format!(
                "Windows Job Object terminated MCP process tree {process_id} but the child was not reaped: {error}"
            )),
            Err(_) => McpProcessCleanupSummary::failed(format!(
                "Windows Job Object terminated MCP process tree {process_id} but child reap exceeded the bounded grace"
            )),
        };
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
        if let Err(signal_error) = crate::process_group::signal_process_group(process_id, "TERM") {
            return match crate::process_group::process_group_has_live_members(process_id).await {
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
        match crate::process_group::process_group_has_live_members(process_id).await {
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
        if let Err(error) = crate::process_group::signal_process_group(process_id, "KILL") {
            return McpProcessCleanupSummary::failed(format!(
                "{reason}; failed to kill remaining process-group descendants: {error}"
            ));
        }
        for _ in 0..20 {
            match crate::process_group::process_group_has_live_members(process_id).await {
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
        return match sigil_process::terminate_owned_process_tree(process_id) {
            Ok(()) => McpProcessCleanupSummary::completed(format!(
                "{reason}; Windows Job Object terminated remaining MCP descendants"
            )),
            Err(error) => McpProcessCleanupSummary::failed(format!(
                "{reason}; failed to terminate remaining MCP descendants through the Windows Job Object: {error}"
            )),
        };
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

fn configure_mcp_process_group(_command: &mut Command) {
    #[cfg(unix)]
    _command.process_group(0);
}

#[cfg(test)]
#[path = "tests/process_tests.rs"]
mod tests;
