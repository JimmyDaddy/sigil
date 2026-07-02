use super::*;

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
    pub env: BTreeMap<String, String>,
    pub startup_timeout_secs: u64,
    pub classification: McpProcessClass,
}

impl McpProcessLaunchRequest {
    pub(super) fn from_config(config: &McpServerConfig, working_dir: Option<PathBuf>) -> Self {
        Self {
            server_name: config.name.clone(),
            command: config.command.clone(),
            args: config.args.clone(),
            working_dir,
            env: BTreeMap::new(),
            startup_timeout_secs: config.startup_timeout_secs,
            classification: McpProcessClass::LocalStdioConfigured,
        }
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
        if let Some(working_dir) = &request.working_dir {
            command.current_dir(working_dir);
        }
        command.envs(&request.env);
        let child = command
            .spawn()
            .with_context(|| format!("failed to spawn MCP server {}", request.server_name))?;
        Ok(McpProcessLaunch {
            child,
            receipt: McpProcessLaunchReceipt::local_outside_sandbox(&request),
        })
    }
}

pub(super) async fn drain_mcp_stderr(mut stderr: ChildStderr) {
    let mut buffer = [0_u8; 4096];
    while stderr.read(&mut buffer).await.is_ok_and(|read| read > 0) {}
}
