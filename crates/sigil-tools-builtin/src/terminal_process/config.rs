use super::*;

pub const TERMINAL_TASK_ARTIFACT_ROOT: &str = "state/artifacts/tasks";
pub(super) const TERMINAL_TASK_META_FILE: &str = "meta.json";
pub(super) const TERMINAL_TASK_OUTPUT_FILE: &str = "output.log";
pub(super) const TERMINAL_TASK_STDOUT_FILE: &str = "stdout.log";
pub(super) const TERMINAL_TASK_STDERR_FILE: &str = "stderr.log";
pub(super) const DEFAULT_TERMINAL_PREVIEW_LIMIT_BYTES: usize = 16 * 1024;
pub(super) const DEFAULT_CANCEL_GRACE_MS: u64 = 500;
const DEFAULT_TERMINAL_PTY_ROWS: u16 = 24;
const DEFAULT_TERMINAL_PTY_COLS: u16 = 80;
pub const MAX_TERMINAL_INPUT_BYTES: usize = 8 * 1024;
pub(super) const TERMINAL_PTY_INPUT_QUEUE_BOUND: usize = 8;
pub(super) const PTY_CANCEL_POLL_INTERVAL_MS: u64 = 20;

/// Terminal backend implementation used for one running task.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalBackendKind {
    Process,
    Pty,
}

impl TerminalBackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::Pty => "pty",
        }
    }
}

/// Portable PTY dimensions.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TerminalPtySize {
    pub rows: u16,
    pub cols: u16,
}

impl TerminalPtySize {
    /// Creates a non-zero PTY size.
    ///
    /// # Errors
    ///
    /// Returns an error when either dimension is zero.
    pub fn new(rows: u16, cols: u16) -> Result<Self> {
        if rows == 0 || cols == 0 {
            bail!("terminal pty rows and cols must be non-zero");
        }
        Ok(Self { rows, cols })
    }

    pub(super) fn to_portable(self) -> PtySize {
        PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

impl Default for TerminalPtySize {
    fn default() -> Self {
        Self {
            rows: DEFAULT_TERMINAL_PTY_ROWS,
            cols: DEFAULT_TERMINAL_PTY_COLS,
        }
    }
}

/// Execution policy used by persistent terminal tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalExecutionConfig {
    backend: ExecutionBackendKind,
    profile: ExecutionSandboxProfile,
    fallback: ExecutionSandboxFallback,
    requires_sandbox: bool,
    network_allowed: bool,
}

impl TerminalExecutionConfig {
    #[must_use]
    pub fn from_execution_config(config: &ExecutionConfig) -> Self {
        Self {
            backend: config.backend(),
            profile: config.profile(),
            fallback: config.fallback(),
            requires_sandbox: config.requires_sandbox(),
            network_allowed: config.profile_spec().network_allowed,
        }
    }

    pub(super) fn resolve_pty_execution(
        &self,
        resolved_cwd: &Path,
        shell: &str,
        command: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<TerminalPtyExecution> {
        match self.backend {
            ExecutionBackendKind::Local => {
                if self.requires_sandbox {
                    return self.fallback_or_error(
                        "local execution backend cannot enforce persistent terminal sandbox",
                    );
                }
                Ok(local_pty_execution(resolved_cwd, shell, command, env))
            }
            ExecutionBackendKind::MacosSeatbelt => {
                self.resolve_macos_seatbelt_pty_execution(resolved_cwd, shell, command, env)
            }
            ExecutionBackendKind::LinuxBubblewrap => {
                self.resolve_linux_bubblewrap_pty_execution(resolved_cwd, shell, command, env)
            }
            ExecutionBackendKind::Docker => self.fallback_or_error(
                "docker execution backend does not support persistent terminal pty",
            ),
        }
    }

    fn resolve_macos_seatbelt_pty_execution(
        &self,
        resolved_cwd: &Path,
        shell: &str,
        command: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<TerminalPtyExecution> {
        let backend = MacosSeatbeltExecutionBackend::default();
        let capabilities = backend.capabilities();
        self.validate_terminal_capabilities(capabilities)?;
        if let Err(error) = ensure_macos_seatbelt_available(&backend) {
            return self.fallback_or_error(error.to_string());
        }

        let profile = macos_seatbelt_workspace_write_profile(resolved_cwd);
        let command_spec = TerminalPtyCommandSpec {
            program: PathBuf::from("/usr/bin/sandbox-exec"),
            args: vec![
                OsString::from("-p"),
                OsString::from(profile),
                OsString::from(shell),
                OsString::from("-lc"),
                OsString::from(command),
            ],
            cwd: resolved_cwd.to_path_buf(),
            env: env.clone(),
        };
        Ok(TerminalPtyExecution::sandboxed(
            ExecutionBackendKind::MacosSeatbelt,
            capabilities,
            self.profile,
            command_spec,
        ))
    }

    fn resolve_linux_bubblewrap_pty_execution(
        &self,
        resolved_cwd: &Path,
        shell: &str,
        command: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<TerminalPtyExecution> {
        let Some(bwrap) = find_executable_on_path("bwrap") else {
            return self
                .fallback_or_error("linux_bubblewrap execution backend requires bwrap on PATH");
        };
        let backend = LinuxBubblewrapExecutionBackend::new(bwrap.clone(), self.network_allowed);
        let capabilities = backend.capabilities();
        self.validate_terminal_capabilities(capabilities)?;
        if let Err(error) = ensure_linux_bubblewrap_available(&backend) {
            return self.fallback_or_error(error.to_string());
        }

        let request = ExecutionRequest {
            program: shell.to_owned(),
            args: vec!["-lc".to_owned(), command.to_owned()],
            cwd: resolved_cwd.to_path_buf(),
            env: env.clone(),
            environment_policy: sigil_kernel::ProcessEnvironmentPolicy::InheritParent,
            timeout_ms: None,
            timeout_secs: 0,
            cpu_time_ms: None,
            memory_limit_bytes: None,
            process_count_limit: None,
        };
        let mut args = linux_bubblewrap_args(resolved_cwd, &request, self.network_allowed);
        args.push(OsString::from(shell));
        args.push(OsString::from("-lc"));
        args.push(OsString::from(command));
        let command_spec = TerminalPtyCommandSpec {
            program: bwrap,
            args,
            cwd: resolved_cwd.to_path_buf(),
            env: env.clone(),
        };
        Ok(TerminalPtyExecution::sandboxed(
            ExecutionBackendKind::LinuxBubblewrap,
            capabilities,
            self.profile,
            command_spec,
        ))
    }

    fn validate_terminal_capabilities(
        &self,
        capabilities: ExecutionBackendCapabilities,
    ) -> Result<()> {
        let config = if self.requires_sandbox {
            let mut sandbox = sigil_kernel::ExecutionSandboxStrategyConfig::new(self.backend);
            sandbox.profile = self.profile;
            sandbox.fallback = self.fallback;
            ExecutionConfig::sandbox(sandbox)
        } else {
            ExecutionConfig::local()
        };
        let requirements = config.required_capabilities_for_persistent_pty();
        let missing = capabilities.missing_requirements(requirements);
        if missing.is_empty() {
            return Ok(());
        }
        self.fallback_or_error(format!(
            "execution backend {} missing persistent terminal capabilities: {}",
            self.backend.as_str(),
            missing
                .iter()
                .map(|capability| capability.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        Ok(())
    }

    fn fallback_or_error<T>(&self, reason: impl Into<String>) -> Result<T> {
        let reason = reason.into();
        match self.fallback {
            ExecutionSandboxFallback::Unconfined => {
                bail!(
                    "persistent terminal sandbox unavailable: {reason}; unconfined fallback is not used for terminal pty tasks"
                )
            }
            ExecutionSandboxFallback::Deny | ExecutionSandboxFallback::Prompt => {
                bail!("persistent terminal sandbox unavailable: {reason}")
            }
        }
    }
}

impl Default for TerminalExecutionConfig {
    fn default() -> Self {
        Self {
            backend: ExecutionBackendKind::Local,
            profile: ExecutionSandboxProfile::Unconfined,
            fallback: ExecutionSandboxFallback::Deny,
            requires_sandbox: false,
            network_allowed: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TerminalPtyExecution {
    pub(super) execution_backend: TerminalExecutionBackendKind,
    pub(super) execution_backend_capabilities: TerminalExecutionBackendCapabilities,
    pub(super) enforcement_backend: ExecutionBackendKind,
    pub(super) enforcement_backend_capabilities: ExecutionBackendCapabilities,
    pub(super) sandbox_profile: ExecutionSandboxProfile,
    pub(super) command: TerminalPtyCommandSpec,
}

impl TerminalPtyExecution {
    fn sandboxed(
        enforcement_backend: ExecutionBackendKind,
        enforcement_backend_capabilities: ExecutionBackendCapabilities,
        sandbox_profile: ExecutionSandboxProfile,
        command: TerminalPtyCommandSpec,
    ) -> Self {
        Self {
            execution_backend: TerminalExecutionBackendKind::SandboxedPty,
            execution_backend_capabilities: TerminalExecutionBackendCapabilities::sandboxed_pty(),
            enforcement_backend,
            enforcement_backend_capabilities,
            sandbox_profile,
            command,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TerminalPtyCommandSpec {
    pub(super) program: PathBuf,
    pub(super) args: Vec<OsString>,
    pub(super) cwd: PathBuf,
    pub(super) env: BTreeMap<String, String>,
}

fn local_pty_execution(
    resolved_cwd: &Path,
    shell: &str,
    command: &str,
    env: &BTreeMap<String, String>,
) -> TerminalPtyExecution {
    TerminalPtyExecution {
        execution_backend: TerminalExecutionBackendKind::LocalPty,
        execution_backend_capabilities: TerminalExecutionBackendCapabilities::local_pty(),
        enforcement_backend: ExecutionBackendKind::Local,
        enforcement_backend_capabilities: ExecutionBackendCapabilities::default(),
        sandbox_profile: ExecutionSandboxProfile::Unconfined,
        command: TerminalPtyCommandSpec {
            program: PathBuf::from(shell),
            args: vec![OsString::from("-lc"), OsString::from(command)],
            cwd: resolved_cwd.to_path_buf(),
            env: env.clone(),
        },
    }
}

/// Request used by the non-PTY terminal process backend.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalStartRequest {
    pub task_id: Option<TerminalTaskId>,
    pub command: String,
    pub cwd: Option<PathBuf>,
    pub shell: Option<String>,
    pub env: BTreeMap<String, String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_entry: Option<TerminalTaskEntry>,
    pub content: String,
    pub returned_bytes: u64,
    pub total_bytes: u64,
    pub truncated: bool,
}

/// Result for data written to a running terminal task stdin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TerminalInputResult {
    pub task_id: TerminalTaskId,
    pub input_bytes: u64,
    pub backend: TerminalBackendKind,
}

/// Synchronous permission context for a live terminal task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalTaskPermissionContext {
    pub task_id: TerminalTaskId,
    pub command: String,
    pub cwd: PathBuf,
    pub shell: String,
}

/// Result for a terminal task resize operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TerminalResizeResult {
    pub task_id: TerminalTaskId,
    pub size: TerminalPtySize,
    pub backend: TerminalBackendKind,
}
